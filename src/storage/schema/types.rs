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

/// Structured SQL type name used by the parser/analyzer boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlTypeName {
    pub name: String,
    pub modifiers: Vec<TypeModifier>,
}

impl SqlTypeName {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            modifiers: Vec::new(),
        }
    }

    pub fn simple(name: impl Into<String>) -> Self {
        Self::new(name)
    }

    pub fn with_modifiers(mut self, modifiers: Vec<TypeModifier>) -> Self {
        self.modifiers = modifiers;
        self
    }

    pub fn base_name(&self) -> String {
        self.name.to_ascii_uppercase()
    }

    pub fn parse_declared(input: &str) -> Self {
        parse_sql_type_name(input).unwrap_or_else(|| Self::simple(input.trim()))
    }

    pub fn enum_variants(&self) -> Option<Vec<String>> {
        if self.base_name() != "ENUM" {
            return None;
        }
        let mut variants = Vec::new();
        for modifier in &self.modifiers {
            if let TypeModifier::StringLiteral(value) = modifier {
                variants.push(value.clone());
            } else {
                return None;
            }
        }
        Some(variants)
    }

    pub fn array_element_type(&self) -> Option<String> {
        if self.base_name() != "ARRAY" {
            return None;
        }
        self.modifiers.iter().find_map(|modifier| match modifier {
            TypeModifier::Type(inner) => Some(inner.to_string()),
            TypeModifier::Ident(name) => Some(name.to_ascii_uppercase()),
            _ => None,
        })
    }

    pub fn decimal_precision(&self) -> Option<u8> {
        match self.base_name().as_str() {
            "DECIMAL" | "NUMERIC" => self.modifiers.iter().find_map(|modifier| match modifier {
                TypeModifier::Number(value) => u8::try_from(*value).ok(),
                _ => None,
            }),
            _ => None,
        }
    }
}

impl fmt::Display for SqlTypeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.base_name())?;
        if !self.modifiers.is_empty() {
            write!(f, "(")?;
            for (idx, modifier) in self.modifiers.iter().enumerate() {
                if idx > 0 {
                    write!(f, ",")?;
                }
                write!(f, "{modifier}")?;
            }
            write!(f, ")")?;
        }
        Ok(())
    }
}

/// Type modifiers used by SQL types like `DECIMAL(10)` or `ARRAY(TEXT)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeModifier {
    Number(u32),
    Ident(String),
    StringLiteral(String),
    Type(Box<SqlTypeName>),
}

impl fmt::Display for TypeModifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(value) => write!(f, "{value}"),
            Self::Ident(value) => write!(f, "{}", value.to_ascii_uppercase()),
            Self::StringLiteral(value) => write!(f, "'{value}'"),
            Self::Type(value) => write!(f, "{value}"),
        }
    }
}

fn parse_sql_type_name(input: &str) -> Option<SqlTypeName> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }

    let open = input.find('(');
    let close = input.rfind(')');
    match (open, close) {
        (Some(open), Some(close)) if close > open => {
            let name = input[..open].trim();
            let inner = &input[open + 1..close];
            let modifiers = split_type_modifiers(inner)
                .into_iter()
                .map(parse_type_modifier)
                .collect::<Option<Vec<_>>>()?;
            Some(SqlTypeName::new(name).with_modifiers(modifiers))
        }
        _ => Some(SqlTypeName::new(input)),
    }
}

fn parse_type_modifier(input: String) -> Option<TypeModifier> {
    let value = input.trim();
    if value.is_empty() {
        return None;
    }
    if value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2 {
        return Some(TypeModifier::StringLiteral(
            value[1..value.len() - 1].to_string(),
        ));
    }
    if let Ok(number) = value.parse::<u32>() {
        return Some(TypeModifier::Number(number));
    }
    if value.contains('(') {
        return parse_sql_type_name(value).map(|inner| TypeModifier::Type(Box::new(inner)));
    }
    Some(TypeModifier::Ident(value.to_string()))
}

fn split_type_modifiers(input: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    let mut in_string = false;

    for ch in input.chars() {
        match ch {
            '\'' => {
                in_string = !in_string;
                current.push(ch);
            }
            '(' if !in_string => {
                depth += 1;
                current.push(ch);
            }
            ')' if !in_string => {
                depth = depth.saturating_sub(1);
                current.push(ch);
            }
            ',' if !in_string && depth == 0 => {
                parts.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }

    parts
}

/// Type identifier for column definitions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum DataType {
    /// Polymorphic / "any" placeholder. Used by function-catalog
    /// signatures (e.g. `JSON_SET(json, path, value)` where `value`
    /// accepts any scalar) so the resolver can skip concrete type
    /// checking for that argument. Never appears in stored schemas.
    Unknown = 0,
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
    /// RGB color (3 bytes)
    Color = 19,
    /// Email address (validated, stored lowercase)
    Email = 20,
    /// URL (validated)
    Url = 21,
    /// Phone number (stored as u64 digits)
    Phone = 22,
    /// Semantic version (packed u32: major*1M + minor*1K + patch)
    Semver = 23,
    /// CIDR notation (IPv4 u32 + prefix u8 = 5 bytes)
    Cidr = 24,
    /// Date only (i32 days since Unix epoch, no time)
    Date = 25,
    /// Time only (u32 milliseconds since midnight)
    Time = 26,
    /// Fixed-point decimal (i64 with configurable precision)
    Decimal = 27,
    /// Enumerated type (u8 index into variant list)
    Enum = 28,
    /// Array of values (homogeneous)
    Array = 29,
    /// Timestamp with millisecond precision (i64 ms since epoch)
    TimestampMs = 30,
    /// IPv4 address (u32)
    Ipv4 = 31,
    /// IPv6 address ([u8; 16])
    Ipv6 = 32,
    /// Network subnet (ip u32 + mask u32)
    Subnet = 33,
    /// TCP/UDP port number (u16)
    Port = 34,
    /// Geographic latitude (i32 microdegrees)
    Latitude = 35,
    /// Geographic longitude (i32 microdegrees)
    Longitude = 36,
    /// Geographic point (lat i32 + lon i32)
    GeoPoint = 37,
    /// ISO 3166-1 alpha-2 country code ([u8; 2])
    Country2 = 38,
    /// ISO 3166-1 alpha-3 country code ([u8; 3])
    Country3 = 39,
    /// ISO 639-1 language code ([u8; 2])
    Lang2 = 40,
    /// IETF language tag, e.g. "pt-BR" ([u8; 5])
    Lang5 = 41,
    /// ISO 4217 currency code ([u8; 3])
    Currency = 42,
    /// RGBA color with alpha ([u8; 4])
    ColorAlpha = 43,
    /// Signed 64-bit integer (alias for large numbers)
    BigInt = 44,
    /// Reference to a KV pair (collection, key string)
    KeyRef = 45,
    /// Reference to a document (collection, entity_id)
    DocRef = 46,
    /// Reference to a table/collection by name
    TableRef = 47,
    /// Reference to a physical storage page
    PageRef = 48,
    /// Encrypted secret (AES-256-GCM ciphertext, keyed by vault AES key)
    Secret = 49,
    /// Argon2id password hash
    Password = 50,
    /// C3 TOAST: zstd-compressed UTF-8 text (> TOAST_THRESHOLD bytes).
    /// In-memory representation is always `Value::Text` — compression is
    /// transparent to all callers above the serialization layer.
    TextZstd = 51,
    /// C3 TOAST: zstd-compressed binary blob (> TOAST_THRESHOLD bytes).
    /// In-memory representation is always `Value::Blob`.
    BlobZstd = 52,
    /// General asset code (fiat or crypto), validated and normalized uppercase text
    AssetCode = 53,
    /// Monetary amount represented as minor units + scale + asset code
    Money = 54,
}

/// Type categories used by the Fase 3 coercion resolver. Mirrors
/// PostgreSQL's `pg_type.h` `typcategory` values. Types in the same
/// category are candidates for implicit coercion; the preferred flag
/// breaks ties when multiple candidates match an operator / function
/// signature.
///
/// See the Fase 3 algorithm described in the roadmap file
/// (`/home/cyber/.claude/plans/squishy-mixing-honey.md`, Parte 4)
/// for the full candidate-selection heuristic. This enum is the
/// decoupler that makes that algorithm tractable — without
/// categories you'd need O(n²) pairwise coercion rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeCategory {
    /// Integer, float, decimal — widening promotes int → float → numeric.
    Numeric,
    /// Text, blob, enum labels — stringy families that freely coerce
    /// to text via display.
    String,
    /// Boolean only.
    Boolean,
    /// Date, time, timestamp — temporal values that share arithmetic
    /// (`date + interval`, `timestamp - timestamp`, etc.).
    DateTime,
    /// Duration / interval.
    TimeSpan,
    /// Array of any element type. The element's own category is
    /// tracked on `DataType::Array` itself; this top-level marker
    /// distinguishes array-typed operands from scalar ones.
    Array,
    /// Network addresses (IPv4, IPv6, CIDR, subnet, MAC).
    Network,
    /// Geographic / spatial types (lat / lon / geopoint).
    Geo,
    /// Domain-typed values (email, URL, phone, semver, color, locale codes)
    /// — these are all conceptually strings with extra validation, but
    /// they get their own category so the coercion resolver can prefer
    /// their native equality over `text` equality.
    Domain,
    /// UUID — distinct category because neither string nor numeric
    /// coercion makes sense.
    Uuid,
    /// Encrypted / protected values (Secret, Password). Callers must
    /// opt in to any coercion; the resolver treats this category as
    /// opaque.
    Opaque,
    /// Reference to another entity / page / table — opaque to the
    /// expression layer.
    Reference,
    /// Vector of floats.
    Vector,
    /// Embedded JSON blob.
    Json,
    /// Null / unknown — used as the sentinel for untyped literals
    /// before the analyzer resolves a concrete type.
    Unknown,
}

impl DataType {
    /// Return the coercion category this type belongs to. Used by
    /// the Fase 3 operator / function resolver to group candidate
    /// types when picking an overload.
    pub fn category(&self) -> TypeCategory {
        match self {
            DataType::Integer
            | DataType::UnsignedInteger
            | DataType::Float
            | DataType::Decimal
            | DataType::BigInt
            | DataType::Port
            | DataType::Latitude
            | DataType::Longitude => TypeCategory::Numeric,
            DataType::Text | DataType::Blob => TypeCategory::String,
            DataType::Boolean => TypeCategory::Boolean,
            DataType::Timestamp | DataType::TimestampMs | DataType::Date | DataType::Time => {
                TypeCategory::DateTime
            }
            DataType::Duration => TypeCategory::TimeSpan,
            DataType::Array => TypeCategory::Array,
            DataType::IpAddr
            | DataType::Ipv4
            | DataType::Ipv6
            | DataType::Cidr
            | DataType::Subnet
            | DataType::MacAddr => TypeCategory::Network,
            DataType::GeoPoint => TypeCategory::Geo,
            DataType::Email
            | DataType::Url
            | DataType::Phone
            | DataType::Semver
            | DataType::Color
            | DataType::ColorAlpha
            | DataType::Country2
            | DataType::Country3
            | DataType::Lang2
            | DataType::Lang5
            | DataType::Currency
            | DataType::AssetCode
            | DataType::Money
            | DataType::Enum => TypeCategory::Domain,
            DataType::Uuid => TypeCategory::Uuid,
            DataType::Secret | DataType::Password => TypeCategory::Opaque,
            DataType::NodeRef
            | DataType::EdgeRef
            | DataType::VectorRef
            | DataType::RowRef
            | DataType::KeyRef
            | DataType::DocRef
            | DataType::TableRef
            | DataType::PageRef => TypeCategory::Reference,
            DataType::Vector => TypeCategory::Vector,
            DataType::Json => TypeCategory::Json,
            DataType::Nullable | DataType::Unknown => TypeCategory::Unknown,
            // C3 TOAST compressed variants — same category as their uncompressed originals.
            // Callers see Value::Text / Value::Blob after decompression; the DataType is an
            // on-disk-only detail.
            DataType::TextZstd => TypeCategory::String,
            DataType::BlobZstd => TypeCategory::String,
        }
    }

    /// Is this type the *preferred* representative of its category?
    /// When the Fase 3 resolver has two equally-good candidates after
    /// exact-match counting, it picks the preferred one. Preferences
    /// are:
    ///
    /// - Numeric: `Float` (highest precision, captures all integer
    ///   values lossily for arithmetic purposes)
    /// - String: `Text`
    /// - DateTime: `TimestampMs` (milli precision beats second)
    /// - Network: `IpAddr` (superset of Ipv4/Ipv6)
    /// - Domain / Reference / Opaque / others: no preferred member
    ///   (categories where every type is equally "native" so a tie
    ///   means the user must be explicit).
    pub fn is_preferred(&self) -> bool {
        matches!(
            self,
            DataType::Float
                | DataType::Text
                | DataType::TimestampMs
                | DataType::IpAddr
                | DataType::Boolean
                | DataType::Uuid
        )
    }
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
            19 => Some(DataType::Color),
            20 => Some(DataType::Email),
            21 => Some(DataType::Url),
            22 => Some(DataType::Phone),
            23 => Some(DataType::Semver),
            24 => Some(DataType::Cidr),
            25 => Some(DataType::Date),
            26 => Some(DataType::Time),
            27 => Some(DataType::Decimal),
            28 => Some(DataType::Enum),
            29 => Some(DataType::Array),
            30 => Some(DataType::TimestampMs),
            31 => Some(DataType::Ipv4),
            32 => Some(DataType::Ipv6),
            33 => Some(DataType::Subnet),
            34 => Some(DataType::Port),
            35 => Some(DataType::Latitude),
            36 => Some(DataType::Longitude),
            37 => Some(DataType::GeoPoint),
            38 => Some(DataType::Country2),
            39 => Some(DataType::Country3),
            40 => Some(DataType::Lang2),
            41 => Some(DataType::Lang5),
            42 => Some(DataType::Currency),
            43 => Some(DataType::ColorAlpha),
            44 => Some(DataType::BigInt),
            45 => Some(DataType::KeyRef),
            46 => Some(DataType::DocRef),
            47 => Some(DataType::TableRef),
            48 => Some(DataType::PageRef),
            49 => Some(DataType::Secret),
            50 => Some(DataType::Password),
            51 => Some(DataType::TextZstd),
            52 => Some(DataType::BlobZstd),
            53 => Some(DataType::AssetCode),
            54 => Some(DataType::Money),
            _ => None,
        }
    }

    /// Resolve a DataType from an SQL-level type name (case-insensitive).
    /// Accepts short SQL aliases (INT → Integer, STRING → Text) plus the
    /// canonical reddb names rendered by `Display`. Returns `None` for
    /// unknown / unsupported names so callers can surface a parse error.
    pub fn from_sql_name(name: &str) -> Option<Self> {
        Self::from_sql_type_name(&SqlTypeName::parse_declared(name))
    }

    /// Resolve a DataType from a parsed SQL type name.
    pub fn from_sql_type_name(sql_type: &SqlTypeName) -> Option<Self> {
        let n = sql_type.base_name();
        Some(match n.as_str() {
            "BOOL" | "BOOLEAN" => DataType::Boolean,
            "INT" | "INTEGER" | "INT4" | "SERIAL" => DataType::Integer,
            "INT2" | "SMALLINT" => DataType::Integer,
            "INT8" | "BIGINT" | "BIGINT_SIGNED" | "BIGSERIAL" => DataType::BigInt,
            "UINT" | "UNSIGNED" | "UNSIGNED_INTEGER" | "UNSIGNED INTEGER" => {
                DataType::UnsignedInteger
            }
            "FLOAT" | "DOUBLE" | "REAL" | "FLOAT8" => DataType::Float,
            "TEXT" | "STRING" | "VARCHAR" | "CHAR" => DataType::Text,
            "BLOB" | "BYTES" | "BYTEA" => DataType::Blob,
            "TIMESTAMP" => DataType::Timestamp,
            "TIMESTAMPTZ" | "TIMESTAMPMS" | "TIMESTAMP_MS" => DataType::TimestampMs,
            "DURATION" | "INTERVAL" => DataType::Duration,
            "DATE" => DataType::Date,
            "TIME" => DataType::Time,
            "DECIMAL" | "NUMERIC" => DataType::Decimal,
            "JSON" | "JSONB" => DataType::Json,
            "UUID" => DataType::Uuid,
            "IPADDR" | "IP" | "INET" => DataType::IpAddr,
            "IPV4" => DataType::Ipv4,
            "IPV6" => DataType::Ipv6,
            "MACADDR" => DataType::MacAddr,
            "NODEREF" => DataType::NodeRef,
            "EDGEREF" => DataType::EdgeRef,
            "VECTORREF" => DataType::VectorRef,
            "ROWREF" => DataType::RowRef,
            "CIDR" => DataType::Cidr,
            "SUBNET" => DataType::Subnet,
            "PORT" => DataType::Port,
            "COLOR" => DataType::Color,
            "COLOR_ALPHA" | "COLORALPHA" => DataType::ColorAlpha,
            "EMAIL" => DataType::Email,
            "URL" => DataType::Url,
            "PHONE" => DataType::Phone,
            "SEMVER" => DataType::Semver,
            "LATITUDE" => DataType::Latitude,
            "LONGITUDE" => DataType::Longitude,
            "GEOPOINT" | "GEO_POINT" => DataType::GeoPoint,
            "COUNTRY2" => DataType::Country2,
            "COUNTRY3" => DataType::Country3,
            "LANG2" => DataType::Lang2,
            "LANG5" => DataType::Lang5,
            "CURRENCY" => DataType::Currency,
            "ASSETCODE" | "ASSET_CODE" | "ASSET" => DataType::AssetCode,
            "MONEY" => DataType::Money,
            "ENUM" => DataType::Enum,
            "ARRAY" => DataType::Array,
            "KEYREF" => DataType::KeyRef,
            "DOCREF" => DataType::DocRef,
            "TABLEREF" => DataType::TableRef,
            "PAGEREF" => DataType::PageRef,
            "SECRET" => DataType::Secret,
            "PASSWORD" => DataType::Password,
            "VECTOR" => DataType::Vector,
            _ => return None,
        })
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
            DataType::Unknown => None,
            DataType::Json => None,
            // Cross-references (variable-length IDs)
            DataType::NodeRef => None,
            DataType::EdgeRef => None,
            DataType::VectorRef => Some(8),   // u64 vector ID
            DataType::RowRef => None,         // table_id (varint) + row_id (u64)
            DataType::Color => Some(3),       // RGB
            DataType::Email => None,          // variable-length string
            DataType::Url => None,            // variable-length string
            DataType::Phone => Some(8),       // u64
            DataType::Semver => Some(4),      // u32
            DataType::Cidr => Some(5),        // u32 + u8
            DataType::Date => Some(4),        // i32
            DataType::Time => Some(4),        // u32
            DataType::Decimal => Some(8),     // i64
            DataType::Enum => Some(1),        // u8
            DataType::Array => None,          // variable-length
            DataType::TimestampMs => Some(8), // i64
            DataType::Ipv4 => Some(4),        // u32
            DataType::Ipv6 => Some(16),       // [u8; 16]
            DataType::Subnet => Some(8),      // u32 + u32
            DataType::Port => Some(2),        // u16
            DataType::Latitude => Some(4),    // i32
            DataType::Longitude => Some(4),   // i32
            DataType::GeoPoint => Some(8),    // i32 + i32
            DataType::Country2 => Some(2),    // [u8; 2]
            DataType::Country3 => Some(3),    // [u8; 3]
            DataType::Lang2 => Some(2),       // [u8; 2]
            DataType::Lang5 => Some(5),       // [u8; 5]
            DataType::Currency => Some(3),    // [u8; 3]
            DataType::ColorAlpha => Some(4),  // [u8; 4]
            DataType::BigInt => Some(8),      // i64
            DataType::AssetCode => None,      // variable-length normalized code
            DataType::Money => None,          // variable-length asset + scale + i64
            DataType::KeyRef => None,         // variable-length (collection + key)
            DataType::DocRef => None,         // variable-length (collection + u64)
            DataType::TableRef => None,       // variable-length (table name)
            DataType::PageRef => Some(4),     // u32
            DataType::Secret => None,         // variable-length ciphertext
            DataType::Password => None,       // variable-length hash string
            DataType::TextZstd => None,       // variable-length compressed text
            DataType::BlobZstd => None,       // variable-length compressed blob
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
                | DataType::Email
                | DataType::Url
                | DataType::Phone
                | DataType::Semver
                | DataType::Date
                | DataType::Time
                | DataType::Decimal
                | DataType::Enum
                | DataType::TimestampMs
                | DataType::Ipv4
                | DataType::Ipv6
                | DataType::Port
                | DataType::Latitude
                | DataType::Longitude
                | DataType::GeoPoint
                | DataType::Country2
                | DataType::Country3
                | DataType::Lang2
                | DataType::Lang5
                | DataType::Currency
                | DataType::AssetCode
                | DataType::BigInt
                | DataType::KeyRef
                | DataType::DocRef
                | DataType::TableRef
                | DataType::PageRef
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
                | DataType::Date
                | DataType::Time
                | DataType::Decimal
                | DataType::Semver
                | DataType::TimestampMs
                | DataType::Port
                | DataType::Latitude
                | DataType::Longitude
                | DataType::BigInt
                | DataType::AssetCode
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
            DataType::Unknown => write!(f, "UNKNOWN"),
            DataType::Json => write!(f, "JSON"),
            DataType::Uuid => write!(f, "UUID"),
            DataType::NodeRef => write!(f, "NODEREF"),
            DataType::EdgeRef => write!(f, "EDGEREF"),
            DataType::VectorRef => write!(f, "VECTORREF"),
            DataType::RowRef => write!(f, "ROWREF"),
            DataType::Color => write!(f, "COLOR"),
            DataType::Email => write!(f, "EMAIL"),
            DataType::Url => write!(f, "URL"),
            DataType::Phone => write!(f, "PHONE"),
            DataType::Semver => write!(f, "SEMVER"),
            DataType::Cidr => write!(f, "CIDR"),
            DataType::Date => write!(f, "DATE"),
            DataType::Time => write!(f, "TIME"),
            DataType::Decimal => write!(f, "DECIMAL"),
            DataType::Enum => write!(f, "ENUM"),
            DataType::Array => write!(f, "ARRAY"),
            DataType::TimestampMs => write!(f, "TIMESTAMP_MS"),
            DataType::Ipv4 => write!(f, "IPV4"),
            DataType::Ipv6 => write!(f, "IPV6"),
            DataType::Subnet => write!(f, "SUBNET"),
            DataType::Port => write!(f, "PORT"),
            DataType::Latitude => write!(f, "LATITUDE"),
            DataType::Longitude => write!(f, "LONGITUDE"),
            DataType::GeoPoint => write!(f, "GEOPOINT"),
            DataType::Country2 => write!(f, "COUNTRY2"),
            DataType::Country3 => write!(f, "COUNTRY3"),
            DataType::Lang2 => write!(f, "LANG2"),
            DataType::Lang5 => write!(f, "LANG5"),
            DataType::Currency => write!(f, "CURRENCY"),
            DataType::AssetCode => write!(f, "ASSET_CODE"),
            DataType::Money => write!(f, "MONEY"),
            DataType::ColorAlpha => write!(f, "COLOR_ALPHA"),
            DataType::BigInt => write!(f, "BIGINT"),
            DataType::KeyRef => write!(f, "KEY_REF"),
            DataType::DocRef => write!(f, "DOC_REF"),
            DataType::TableRef => write!(f, "TABLE_REF"),
            DataType::PageRef => write!(f, "PAGE_REF"),
            DataType::Secret => write!(f, "SECRET"),
            DataType::Password => write!(f, "PASSWORD"),
            DataType::TextZstd => write!(f, "TEXT"), // presents as TEXT externally
            DataType::BlobZstd => write!(f, "BLOB"), // presents as BLOB externally
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
    /// RGB color
    Color([u8; 3]),
    /// Email (stored lowercase, validated)
    Email(String),
    /// URL (validated)
    Url(String),
    /// Phone as digits (e.g., +5511999 -> 5511999u64)
    Phone(u64),
    /// Semantic version packed as u32
    Semver(u32),
    /// CIDR (ip as u32, prefix as u8)
    Cidr(u32, u8),
    /// Date as days since Unix epoch
    Date(i32),
    /// Time as milliseconds since midnight
    Time(u32),
    /// Fixed-point decimal (value * 10^precision)
    Decimal(i64),
    /// Enum variant index
    EnumValue(u8),
    /// Homogeneous array
    Array(Vec<Value>),
    /// Timestamp in milliseconds since epoch
    TimestampMs(i64),
    /// IPv4 as u32
    Ipv4(u32),
    /// IPv6 as 16 bytes
    Ipv6([u8; 16]),
    /// Subnet: ip(u32) + mask(u32)
    Subnet(u32, u32),
    /// Port number
    Port(u16),
    /// Latitude in microdegrees (degrees * 1_000_000)
    Latitude(i32),
    /// Longitude in microdegrees
    Longitude(i32),
    /// GeoPoint (lat, lon) in microdegrees
    GeoPoint(i32, i32),
    /// ISO country code 2-letter
    Country2([u8; 2]),
    /// ISO country code 3-letter
    Country3([u8; 3]),
    /// Language code 2-letter
    Lang2([u8; 2]),
    /// Language tag 5-char (e.g., "pt-BR")
    Lang5([u8; 5]),
    /// Currency code 3-letter
    Currency([u8; 3]),
    /// General asset code, normalized uppercase text
    AssetCode(String),
    /// Monetary amount stored as integer minor units plus explicit scale and asset code
    Money {
        asset_code: String,
        minor_units: i64,
        scale: u8,
    },
    /// RGBA color with alpha
    ColorAlpha([u8; 4]),
    /// Big integer (same as Integer but with distinct type for schema clarity)
    BigInt(i64),
    /// Reference to a KV pair (collection, key)
    KeyRef(String, String),
    /// Reference to a document (collection, entity_id)
    DocRef(String, u64),
    /// Reference to a table/collection by name
    TableRef(String),
    /// Reference to a physical storage page (page_id)
    PageRef(u32),
    /// Encrypted secret (AES-256-GCM ciphertext bytes: nonce + ciphertext + tag)
    Secret(Vec<u8>),
    /// Argon2id password hash string
    Password(String),
}

/// Manual `Eq` impl: consistent with the manual `Hash` impl below.
/// Float NaN is treated as equal to itself (bit-level equality) so that
/// `Value` can be used in `HashSet<Value>` for IN-list optimizations.
/// This diverges from IEEE 754 but is safe for SQL query evaluation.
impl Eq for Value {}

/// Manual `Hash` impl for `Value`.
/// - Floats: use `f64.to_bits()` so the invariant `a == b → hash(a) == hash(b)` holds.
/// - Arrays / nested values: hash each element recursively.
/// - All other variants: delegate to the byte-level representation.
impl std::hash::Hash for Value {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // Discriminant first — ensures different variants never collide
        std::mem::discriminant(self).hash(state);
        match self {
            Value::Null => {}
            Value::Integer(v) => v.hash(state),
            Value::UnsignedInteger(v) => v.hash(state),
            Value::Float(v) => v.to_bits().hash(state),
            Value::Text(v) => v.hash(state),
            Value::Blob(v) => v.hash(state),
            Value::Boolean(v) => v.hash(state),
            Value::Timestamp(v) => v.hash(state),
            Value::Duration(v) => v.hash(state),
            Value::IpAddr(v) => v.hash(state),
            Value::MacAddr(v) => v.hash(state),
            Value::Vector(v) => {
                v.len().hash(state);
                for f in v {
                    f.to_bits().hash(state);
                }
            }
            Value::Json(v) => v.hash(state),
            Value::Uuid(v) => v.hash(state),
            Value::NodeRef(v) => v.hash(state),
            Value::EdgeRef(v) => v.hash(state),
            Value::VectorRef(c, id) => {
                c.hash(state);
                id.hash(state);
            }
            Value::RowRef(c, id) => {
                c.hash(state);
                id.hash(state);
            }
            Value::Color(v) => v.hash(state),
            Value::Email(v) => v.hash(state),
            Value::Url(v) => v.hash(state),
            Value::Phone(v) => v.hash(state),
            Value::Semver(v) => v.hash(state),
            Value::Cidr(ip, prefix) => {
                ip.hash(state);
                prefix.hash(state);
            }
            Value::Date(v) => v.hash(state),
            Value::Time(v) => v.hash(state),
            Value::Decimal(v) => v.hash(state),
            Value::EnumValue(v) => v.hash(state),
            Value::Array(v) => {
                v.len().hash(state);
                for elem in v {
                    elem.hash(state);
                }
            }
            Value::TimestampMs(v) => v.hash(state),
            Value::Ipv4(v) => v.hash(state),
            Value::Ipv6(v) => v.hash(state),
            Value::Subnet(ip, mask) => {
                ip.hash(state);
                mask.hash(state);
            }
            Value::Port(v) => v.hash(state),
            Value::Latitude(v) => v.hash(state),
            Value::Longitude(v) => v.hash(state),
            Value::GeoPoint(lat, lon) => {
                lat.hash(state);
                lon.hash(state);
            }
            Value::Country2(v) => v.hash(state),
            Value::Country3(v) => v.hash(state),
            Value::Lang2(v) => v.hash(state),
            Value::Lang5(v) => v.hash(state),
            Value::Currency(v) => v.hash(state),
            Value::AssetCode(v) => v.hash(state),
            Value::Money {
                asset_code,
                minor_units,
                scale,
            } => {
                asset_code.hash(state);
                minor_units.hash(state);
                scale.hash(state);
            }
            Value::ColorAlpha(v) => v.hash(state),
            Value::BigInt(v) => v.hash(state),
            Value::KeyRef(c, k) => {
                c.hash(state);
                k.hash(state);
            }
            Value::DocRef(c, id) => {
                c.hash(state);
                id.hash(state);
            }
            Value::TableRef(v) => v.hash(state),
            Value::PageRef(v) => v.hash(state),
            Value::Secret(v) => v.hash(state),
            Value::Password(v) => v.hash(state),
        }
    }
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
            Value::Color(_) => DataType::Color,
            Value::Email(_) => DataType::Email,
            Value::Url(_) => DataType::Url,
            Value::Phone(_) => DataType::Phone,
            Value::Semver(_) => DataType::Semver,
            Value::Cidr(_, _) => DataType::Cidr,
            Value::Date(_) => DataType::Date,
            Value::Time(_) => DataType::Time,
            Value::Decimal(_) => DataType::Decimal,
            Value::EnumValue(_) => DataType::Enum,
            Value::Array(_) => DataType::Array,
            Value::TimestampMs(_) => DataType::TimestampMs,
            Value::Ipv4(_) => DataType::Ipv4,
            Value::Ipv6(_) => DataType::Ipv6,
            Value::Subnet(_, _) => DataType::Subnet,
            Value::Port(_) => DataType::Port,
            Value::Latitude(_) => DataType::Latitude,
            Value::Longitude(_) => DataType::Longitude,
            Value::GeoPoint(_, _) => DataType::GeoPoint,
            Value::Country2(_) => DataType::Country2,
            Value::Country3(_) => DataType::Country3,
            Value::Lang2(_) => DataType::Lang2,
            Value::Lang5(_) => DataType::Lang5,
            Value::Currency(_) => DataType::Currency,
            Value::AssetCode(_) => DataType::AssetCode,
            Value::Money { .. } => DataType::Money,
            Value::ColorAlpha(_) => DataType::ColorAlpha,
            Value::BigInt(_) => DataType::BigInt,
            Value::KeyRef(..) => DataType::KeyRef,
            Value::DocRef(..) => DataType::DocRef,
            Value::TableRef(..) => DataType::TableRef,
            Value::PageRef(..) => DataType::PageRef,
            Value::Secret(..) => DataType::Secret,
            Value::Password(..) => DataType::Password,
        }
    }

    /// Check if value is null
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// C3 TOAST: minimum byte length to attempt zstd compression.
    /// Values shorter than this are stored uncompressed — compression
    /// overhead (~50 ns + header bytes) outweighs savings for small values.
    const TOAST_THRESHOLD: usize = 2048;

    /// zstd compression level for TOAST values. Level 3 is PG's default
    /// (balanced speed/ratio). Raise to 6 for write-rarely / read-many
    /// workloads; lower to 1 for high-write, latency-sensitive paths.
    const TOAST_ZSTD_LEVEL: i32 = 3;

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
                let bytes = s.as_bytes();
                // C3 TOAST: compress text values larger than the threshold.
                // Stores with `TextZstd` type byte when compression wins;
                // falls back to plain `Text` for small values or when zstd
                // doesn't reduce the size (e.g. already-compressed content).
                if bytes.len() > Self::TOAST_THRESHOLD {
                    if let Ok(compressed) = zstd::bulk::compress(bytes, Self::TOAST_ZSTD_LEVEL) {
                        if compressed.len() < bytes.len() {
                            buf.push(DataType::TextZstd.to_byte());
                            // original length first (needed to pre-allocate decompression buffer)
                            write_varint(&mut buf, bytes.len() as u64);
                            write_varint(&mut buf, compressed.len() as u64);
                            buf.extend_from_slice(&compressed);
                            return buf;
                        }
                    }
                }
                buf.push(DataType::Text.to_byte());
                write_varint(&mut buf, bytes.len() as u64);
                buf.extend_from_slice(bytes);
            }
            Value::Blob(data) => {
                // C3 TOAST: same pattern as Text.
                if data.len() > Self::TOAST_THRESHOLD {
                    if let Ok(compressed) = zstd::bulk::compress(data, Self::TOAST_ZSTD_LEVEL) {
                        if compressed.len() < data.len() {
                            buf.push(DataType::BlobZstd.to_byte());
                            write_varint(&mut buf, data.len() as u64);
                            write_varint(&mut buf, compressed.len() as u64);
                            buf.extend_from_slice(&compressed);
                            return buf;
                        }
                    }
                }
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
            Value::Color(rgb) => {
                buf.push(DataType::Color.to_byte());
                buf.extend_from_slice(rgb);
            }
            Value::Email(s) => {
                buf.push(DataType::Email.to_byte());
                let bytes = s.as_bytes();
                write_varint(&mut buf, bytes.len() as u64);
                buf.extend_from_slice(bytes);
            }
            Value::Url(s) => {
                buf.push(DataType::Url.to_byte());
                let bytes = s.as_bytes();
                write_varint(&mut buf, bytes.len() as u64);
                buf.extend_from_slice(bytes);
            }
            Value::Phone(n) => {
                buf.push(DataType::Phone.to_byte());
                buf.extend_from_slice(&n.to_le_bytes());
            }
            Value::Semver(packed) => {
                buf.push(DataType::Semver.to_byte());
                buf.extend_from_slice(&packed.to_le_bytes());
            }
            Value::Cidr(ip, prefix) => {
                buf.push(DataType::Cidr.to_byte());
                buf.extend_from_slice(&ip.to_le_bytes());
                buf.push(*prefix);
            }
            Value::Date(days) => {
                buf.push(DataType::Date.to_byte());
                buf.extend_from_slice(&days.to_le_bytes());
            }
            Value::Time(ms) => {
                buf.push(DataType::Time.to_byte());
                buf.extend_from_slice(&ms.to_le_bytes());
            }
            Value::Decimal(v) => {
                buf.push(DataType::Decimal.to_byte());
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Value::EnumValue(idx) => {
                buf.push(DataType::Enum.to_byte());
                buf.push(*idx);
            }
            Value::Array(elements) => {
                buf.push(DataType::Array.to_byte());
                write_varint(&mut buf, elements.len() as u64);
                for elem in elements {
                    let elem_bytes = elem.to_bytes();
                    buf.extend_from_slice(&elem_bytes);
                }
            }
            Value::TimestampMs(v) => {
                buf.push(DataType::TimestampMs.to_byte());
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Value::Ipv4(v) => {
                buf.push(DataType::Ipv4.to_byte());
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Value::Ipv6(bytes) => {
                buf.push(DataType::Ipv6.to_byte());
                buf.extend_from_slice(bytes);
            }
            Value::Subnet(ip, mask) => {
                buf.push(DataType::Subnet.to_byte());
                buf.extend_from_slice(&ip.to_le_bytes());
                buf.extend_from_slice(&mask.to_le_bytes());
            }
            Value::Port(v) => {
                buf.push(DataType::Port.to_byte());
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Value::Latitude(v) => {
                buf.push(DataType::Latitude.to_byte());
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Value::Longitude(v) => {
                buf.push(DataType::Longitude.to_byte());
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Value::GeoPoint(lat, lon) => {
                buf.push(DataType::GeoPoint.to_byte());
                buf.extend_from_slice(&lat.to_le_bytes());
                buf.extend_from_slice(&lon.to_le_bytes());
            }
            Value::Country2(c) => {
                buf.push(DataType::Country2.to_byte());
                buf.extend_from_slice(c);
            }
            Value::Country3(c) => {
                buf.push(DataType::Country3.to_byte());
                buf.extend_from_slice(c);
            }
            Value::Lang2(c) => {
                buf.push(DataType::Lang2.to_byte());
                buf.extend_from_slice(c);
            }
            Value::Lang5(c) => {
                buf.push(DataType::Lang5.to_byte());
                buf.extend_from_slice(c);
            }
            Value::Currency(c) => {
                buf.push(DataType::Currency.to_byte());
                buf.extend_from_slice(c);
            }
            Value::AssetCode(code) => {
                buf.push(DataType::AssetCode.to_byte());
                let bytes = code.as_bytes();
                write_varint(&mut buf, bytes.len() as u64);
                buf.extend_from_slice(bytes);
            }
            Value::Money {
                asset_code,
                minor_units,
                scale,
            } => {
                buf.push(DataType::Money.to_byte());
                let bytes = asset_code.as_bytes();
                write_varint(&mut buf, bytes.len() as u64);
                buf.extend_from_slice(bytes);
                buf.push(*scale);
                buf.extend_from_slice(&minor_units.to_le_bytes());
            }
            Value::ColorAlpha(rgba) => {
                buf.push(DataType::ColorAlpha.to_byte());
                buf.extend_from_slice(rgba);
            }
            Value::BigInt(v) => {
                buf.push(DataType::BigInt.to_byte());
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Value::KeyRef(col, key) => {
                buf.push(DataType::KeyRef.to_byte());
                let col_bytes = col.as_bytes();
                write_varint(&mut buf, col_bytes.len() as u64);
                buf.extend_from_slice(col_bytes);
                let key_bytes = key.as_bytes();
                write_varint(&mut buf, key_bytes.len() as u64);
                buf.extend_from_slice(key_bytes);
            }
            Value::DocRef(col, id) => {
                buf.push(DataType::DocRef.to_byte());
                let col_bytes = col.as_bytes();
                write_varint(&mut buf, col_bytes.len() as u64);
                buf.extend_from_slice(col_bytes);
                buf.extend_from_slice(&id.to_le_bytes());
            }
            Value::TableRef(name) => {
                buf.push(DataType::TableRef.to_byte());
                let name_bytes = name.as_bytes();
                write_varint(&mut buf, name_bytes.len() as u64);
                buf.extend_from_slice(name_bytes);
            }
            Value::PageRef(page_id) => {
                buf.push(DataType::PageRef.to_byte());
                buf.extend_from_slice(&page_id.to_le_bytes());
            }
            Value::Secret(bytes) => {
                buf.push(DataType::Secret.to_byte());
                write_varint(&mut buf, bytes.len() as u64);
                buf.extend_from_slice(bytes);
            }
            Value::Password(hash) => {
                buf.push(DataType::Password.to_byte());
                let bytes = hash.as_bytes();
                write_varint(&mut buf, bytes.len() as u64);
                buf.extend_from_slice(bytes);
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
            DataType::Color => {
                if data.len() < offset + 3 {
                    return Err(ValueError::TruncatedData);
                }
                let rgb: [u8; 3] = data[offset..offset + 3].try_into().unwrap();
                offset += 3;
                Value::Color(rgb)
            }
            DataType::Email => {
                let (len, varint_size) = read_varint(&data[offset..])?;
                offset += varint_size;
                if data.len() < offset + len as usize {
                    return Err(ValueError::TruncatedData);
                }
                let s = String::from_utf8(data[offset..offset + len as usize].to_vec())
                    .map_err(|_| ValueError::InvalidUtf8)?;
                offset += len as usize;
                Value::Email(s)
            }
            DataType::Url => {
                let (len, varint_size) = read_varint(&data[offset..])?;
                offset += varint_size;
                if data.len() < offset + len as usize {
                    return Err(ValueError::TruncatedData);
                }
                let s = String::from_utf8(data[offset..offset + len as usize].to_vec())
                    .map_err(|_| ValueError::InvalidUtf8)?;
                offset += len as usize;
                Value::Url(s)
            }
            DataType::Phone => {
                if data.len() < offset + 8 {
                    return Err(ValueError::TruncatedData);
                }
                let v = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                offset += 8;
                Value::Phone(v)
            }
            DataType::Semver => {
                if data.len() < offset + 4 {
                    return Err(ValueError::TruncatedData);
                }
                let v = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                offset += 4;
                Value::Semver(v)
            }
            DataType::Cidr => {
                if data.len() < offset + 5 {
                    return Err(ValueError::TruncatedData);
                }
                let ip = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                offset += 4;
                let prefix = data[offset];
                offset += 1;
                Value::Cidr(ip, prefix)
            }
            DataType::Date => {
                if data.len() < offset + 4 {
                    return Err(ValueError::TruncatedData);
                }
                let v = i32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                offset += 4;
                Value::Date(v)
            }
            DataType::Time => {
                if data.len() < offset + 4 {
                    return Err(ValueError::TruncatedData);
                }
                let v = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                offset += 4;
                Value::Time(v)
            }
            DataType::Decimal => {
                if data.len() < offset + 8 {
                    return Err(ValueError::TruncatedData);
                }
                let v = i64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                offset += 8;
                Value::Decimal(v)
            }
            DataType::Enum => {
                if data.len() < offset + 1 {
                    return Err(ValueError::TruncatedData);
                }
                let idx = data[offset];
                offset += 1;
                Value::EnumValue(idx)
            }
            DataType::Array => {
                let (len, varint_size) = read_varint(&data[offset..])?;
                offset += varint_size;
                let count = len as usize;
                let mut elements = Vec::with_capacity(count);
                for _ in 0..count {
                    let (elem, elem_size) = Value::from_bytes(&data[offset..])?;
                    offset += elem_size;
                    elements.push(elem);
                }
                Value::Array(elements)
            }
            DataType::TimestampMs => {
                if data.len() < offset + 8 {
                    return Err(ValueError::TruncatedData);
                }
                let v = i64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                offset += 8;
                Value::TimestampMs(v)
            }
            DataType::Ipv4 => {
                if data.len() < offset + 4 {
                    return Err(ValueError::TruncatedData);
                }
                let v = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                offset += 4;
                Value::Ipv4(v)
            }
            DataType::Ipv6 => {
                if data.len() < offset + 16 {
                    return Err(ValueError::TruncatedData);
                }
                let bytes: [u8; 16] = data[offset..offset + 16].try_into().unwrap();
                offset += 16;
                Value::Ipv6(bytes)
            }
            DataType::Subnet => {
                if data.len() < offset + 8 {
                    return Err(ValueError::TruncatedData);
                }
                let ip = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                offset += 4;
                let mask = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                offset += 4;
                Value::Subnet(ip, mask)
            }
            DataType::Port => {
                if data.len() < offset + 2 {
                    return Err(ValueError::TruncatedData);
                }
                let v = u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap());
                offset += 2;
                Value::Port(v)
            }
            DataType::Latitude => {
                if data.len() < offset + 4 {
                    return Err(ValueError::TruncatedData);
                }
                let v = i32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                offset += 4;
                Value::Latitude(v)
            }
            DataType::Longitude => {
                if data.len() < offset + 4 {
                    return Err(ValueError::TruncatedData);
                }
                let v = i32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                offset += 4;
                Value::Longitude(v)
            }
            DataType::GeoPoint => {
                if data.len() < offset + 8 {
                    return Err(ValueError::TruncatedData);
                }
                let lat = i32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                offset += 4;
                let lon = i32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                offset += 4;
                Value::GeoPoint(lat, lon)
            }
            DataType::Country2 => {
                if data.len() < offset + 2 {
                    return Err(ValueError::TruncatedData);
                }
                let c: [u8; 2] = data[offset..offset + 2].try_into().unwrap();
                offset += 2;
                Value::Country2(c)
            }
            DataType::Country3 => {
                if data.len() < offset + 3 {
                    return Err(ValueError::TruncatedData);
                }
                let c: [u8; 3] = data[offset..offset + 3].try_into().unwrap();
                offset += 3;
                Value::Country3(c)
            }
            DataType::Lang2 => {
                if data.len() < offset + 2 {
                    return Err(ValueError::TruncatedData);
                }
                let c: [u8; 2] = data[offset..offset + 2].try_into().unwrap();
                offset += 2;
                Value::Lang2(c)
            }
            DataType::Lang5 => {
                if data.len() < offset + 5 {
                    return Err(ValueError::TruncatedData);
                }
                let c: [u8; 5] = data[offset..offset + 5].try_into().unwrap();
                offset += 5;
                Value::Lang5(c)
            }
            DataType::Currency => {
                if data.len() < offset + 3 {
                    return Err(ValueError::TruncatedData);
                }
                let c: [u8; 3] = data[offset..offset + 3].try_into().unwrap();
                offset += 3;
                Value::Currency(c)
            }
            DataType::AssetCode => {
                let (len, len_bytes) = read_varint(&data[offset..])?;
                offset += len_bytes;
                if data.len() < offset + len as usize {
                    return Err(ValueError::TruncatedData);
                }
                let code = String::from_utf8(data[offset..offset + len as usize].to_vec())
                    .map_err(|_| ValueError::InvalidUtf8)?;
                offset += len as usize;
                Value::AssetCode(code)
            }
            DataType::Money => {
                let (len, len_bytes) = read_varint(&data[offset..])?;
                offset += len_bytes;
                if data.len() < offset + len as usize + 1 + 8 {
                    return Err(ValueError::TruncatedData);
                }
                let asset_code = String::from_utf8(data[offset..offset + len as usize].to_vec())
                    .map_err(|_| ValueError::InvalidUtf8)?;
                offset += len as usize;
                let scale = data[offset];
                offset += 1;
                let minor_units = i64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                offset += 8;
                Value::Money {
                    asset_code,
                    minor_units,
                    scale,
                }
            }
            DataType::ColorAlpha => {
                if data.len() < offset + 4 {
                    return Err(ValueError::TruncatedData);
                }
                let rgba: [u8; 4] = data[offset..offset + 4].try_into().unwrap();
                offset += 4;
                Value::ColorAlpha(rgba)
            }
            DataType::BigInt => {
                if data.len() < offset + 8 {
                    return Err(ValueError::TruncatedData);
                }
                let v = i64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                offset += 8;
                Value::BigInt(v)
            }
            DataType::KeyRef => {
                let (col_len, col_varint) = read_varint(&data[offset..])?;
                offset += col_varint;
                if data.len() < offset + col_len as usize {
                    return Err(ValueError::TruncatedData);
                }
                let col = String::from_utf8(data[offset..offset + col_len as usize].to_vec())
                    .map_err(|_| ValueError::InvalidUtf8)?;
                offset += col_len as usize;
                let (key_len, key_varint) = read_varint(&data[offset..])?;
                offset += key_varint;
                if data.len() < offset + key_len as usize {
                    return Err(ValueError::TruncatedData);
                }
                let key = String::from_utf8(data[offset..offset + key_len as usize].to_vec())
                    .map_err(|_| ValueError::InvalidUtf8)?;
                offset += key_len as usize;
                Value::KeyRef(col, key)
            }
            DataType::DocRef => {
                let (col_len, col_varint) = read_varint(&data[offset..])?;
                offset += col_varint;
                if data.len() < offset + col_len as usize + 8 {
                    return Err(ValueError::TruncatedData);
                }
                let col = String::from_utf8(data[offset..offset + col_len as usize].to_vec())
                    .map_err(|_| ValueError::InvalidUtf8)?;
                offset += col_len as usize;
                let id = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                offset += 8;
                Value::DocRef(col, id)
            }
            DataType::TableRef => {
                let (len, varint_size) = read_varint(&data[offset..])?;
                offset += varint_size;
                if data.len() < offset + len as usize {
                    return Err(ValueError::TruncatedData);
                }
                let name = String::from_utf8(data[offset..offset + len as usize].to_vec())
                    .map_err(|_| ValueError::InvalidUtf8)?;
                offset += len as usize;
                Value::TableRef(name)
            }
            DataType::PageRef => {
                if data.len() < offset + 4 {
                    return Err(ValueError::TruncatedData);
                }
                let page_id = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                offset += 4;
                Value::PageRef(page_id)
            }
            DataType::Secret => {
                let (len, varint_size) = read_varint(&data[offset..])?;
                offset += varint_size;
                if data.len() < offset + len as usize {
                    return Err(ValueError::TruncatedData);
                }
                let bytes = data[offset..offset + len as usize].to_vec();
                offset += len as usize;
                Value::Secret(bytes)
            }
            DataType::Password => {
                let (len, varint_size) = read_varint(&data[offset..])?;
                offset += varint_size;
                if data.len() < offset + len as usize {
                    return Err(ValueError::TruncatedData);
                }
                let hash = String::from_utf8(data[offset..offset + len as usize].to_vec())
                    .map_err(|_| ValueError::InvalidUtf8)?;
                offset += len as usize;
                Value::Password(hash)
            }
            DataType::Nullable => {
                // Nullable without inner type means null
                Value::Null
            }
            DataType::Unknown => {
                // Polymorphic placeholder — never stored on disk.
                // Reaching here means corrupted data or a bug; treat
                // as null to stay forward-compatible.
                Value::Null
            }
            // C3 TOAST: zstd-compressed Text — transparent decompression.
            // Wire: to_bytes writes TextZstd when text > TOAST_THRESHOLD and
            // compression saves space; from_bytes always materialises as Value::Text.
            DataType::TextZstd => {
                let (orig_len, vs1) = read_varint(&data[offset..])?;
                offset += vs1;
                let (comp_len, vs2) = read_varint(&data[offset..])?;
                offset += vs2;
                if data.len() < offset + comp_len as usize {
                    return Err(ValueError::TruncatedData);
                }
                let compressed = &data[offset..offset + comp_len as usize];
                let mut out = vec![0u8; orig_len as usize];
                zstd::bulk::decompress_to_buffer(compressed, &mut out)
                    .map_err(|_| ValueError::InvalidUtf8)?;
                offset += comp_len as usize;
                let s = String::from_utf8(out).map_err(|_| ValueError::InvalidUtf8)?;
                Value::Text(s)
            }
            // C3 TOAST: zstd-compressed Blob — same pattern as TextZstd.
            DataType::BlobZstd => {
                let (orig_len, vs1) = read_varint(&data[offset..])?;
                offset += vs1;
                let (comp_len, vs2) = read_varint(&data[offset..])?;
                offset += vs2;
                if data.len() < offset + comp_len as usize {
                    return Err(ValueError::TruncatedData);
                }
                let compressed = &data[offset..offset + comp_len as usize];
                let mut out = vec![0u8; orig_len as usize];
                zstd::bulk::decompress_to_buffer(compressed, &mut out)
                    .map_err(|_| ValueError::InvalidUtf8)?;
                offset += comp_len as usize;
                Value::Blob(out)
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

    /// Human-readable display string for rich types
    pub fn display_string(&self) -> String {
        match self {
            Value::Color([r, g, b]) => format!("#{:02X}{:02X}{:02X}", r, g, b),
            Value::Email(s) => s.clone(),
            Value::Url(s) => s.clone(),
            Value::Phone(n) => format!("+{}", n),
            Value::Semver(packed) => format!(
                "{}.{}.{}",
                packed / 1_000_000,
                (packed / 1_000) % 1_000,
                packed % 1_000
            ),
            Value::Cidr(ip, prefix) => format!(
                "{}.{}.{}.{}/{}",
                (ip >> 24) & 0xFF,
                (ip >> 16) & 0xFF,
                (ip >> 8) & 0xFF,
                ip & 0xFF,
                prefix
            ),
            Value::Date(days) => format_civil_date(*days),
            Value::Time(ms) => {
                let total_secs = ms / 1000;
                format!(
                    "{:02}:{:02}:{:02}",
                    total_secs / 3600,
                    (total_secs / 60) % 60,
                    total_secs % 60
                )
            }
            Value::Decimal(v) => format_scaled_i64(*v, 4),
            Value::EnumValue(i) => format!("enum({})", i),
            Value::Array(elems) => {
                let items: Vec<String> = elems.iter().map(|e| e.display_string()).collect();
                format!("[{}]", items.join(", "))
            }
            Value::TimestampMs(ms) => {
                let secs = ms / 1000;
                let millis = (ms % 1000).unsigned_abs() as u32;
                let days = (secs / 86400) as i32;
                let day_secs = (secs % 86400) as u32;
                let h = day_secs / 3600;
                let m = (day_secs / 60) % 60;
                let s = day_secs % 60;
                format!(
                    "{}T{:02}:{:02}:{:02}.{:03}Z",
                    format_civil_date(days),
                    h,
                    m,
                    s,
                    millis
                )
            }
            Value::Ipv4(ip) => format!(
                "{}.{}.{}.{}",
                (ip >> 24) & 0xFF,
                (ip >> 16) & 0xFF,
                (ip >> 8) & 0xFF,
                ip & 0xFF
            ),
            Value::Ipv6(bytes) => {
                let addr = std::net::Ipv6Addr::from(*bytes);
                format!("{}", addr)
            }
            Value::Subnet(ip, mask) => {
                let ip_str = format!(
                    "{}.{}.{}.{}",
                    (ip >> 24) & 0xFF,
                    (ip >> 16) & 0xFF,
                    (ip >> 8) & 0xFF,
                    ip & 0xFF
                );
                // Convert mask to prefix length if possible
                let prefix = mask.leading_ones();
                if prefix < 32 && (*mask << prefix) == 0 || prefix == 32 {
                    format!("{}/{}", ip_str, prefix)
                } else {
                    let mask_str = format!(
                        "{}.{}.{}.{}",
                        (mask >> 24) & 0xFF,
                        (mask >> 16) & 0xFF,
                        (mask >> 8) & 0xFF,
                        mask & 0xFF
                    );
                    format!("{}/{}", ip_str, mask_str)
                }
            }
            Value::Port(p) => p.to_string(),
            Value::Latitude(micro) => format!("{:.6}", *micro as f64 / 1_000_000.0),
            Value::Longitude(micro) => format!("{:.6}", *micro as f64 / 1_000_000.0),
            Value::GeoPoint(lat, lon) => format!(
                "{:.6},{:.6}",
                *lat as f64 / 1_000_000.0,
                *lon as f64 / 1_000_000.0
            ),
            Value::Country2(c) => String::from_utf8_lossy(c).to_string(),
            Value::Country3(c) => String::from_utf8_lossy(c).to_string(),
            Value::Lang2(c) => String::from_utf8_lossy(c).to_string(),
            Value::Lang5(c) => String::from_utf8_lossy(c).to_string(),
            Value::Currency(c) => String::from_utf8_lossy(c).to_string(),
            Value::AssetCode(code) => code.clone(),
            Value::Money {
                asset_code,
                minor_units,
                scale,
            } => format!("{} {}", asset_code, format_scaled_i64(*minor_units, *scale)),
            Value::ColorAlpha([r, g, b, a]) => format!("#{:02X}{:02X}{:02X}{:02X}", r, g, b, a),
            Value::BigInt(v) => v.to_string(),
            Value::KeyRef(c, k) => format!("{}:{}", c, k),
            Value::DocRef(c, id) => format!("{}#{}", c, id),
            Value::TableRef(t) => t.clone(),
            Value::PageRef(p) => format!("page:{}", p),
            other => format!("{}", other),
        }
    }
}

fn format_scaled_i64(value: i64, scale: u8) -> String {
    let negative = value < 0;
    let abs = (value as i128).abs();
    if scale == 0 {
        return if negative {
            format!("-{}", abs)
        } else {
            abs.to_string()
        };
    }

    let divisor = 10_i128.pow(scale as u32);
    let whole = abs / divisor;
    let frac = abs % divisor;
    let sign = if negative { "-" } else { "" };
    format!("{}{}.{:0width$}", sign, whole, frac, width = scale as usize)
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
            Value::Color([r, g, b]) => write!(f, "#{:02X}{:02X}{:02X}", r, g, b),
            Value::Email(s) => write!(f, "{}", s),
            Value::Url(s) => write!(f, "{}", s),
            Value::Phone(n) => write!(f, "+{}", n),
            Value::Semver(packed) => write!(
                f,
                "{}.{}.{}",
                packed / 1_000_000,
                (packed / 1_000) % 1_000,
                packed % 1_000
            ),
            Value::Cidr(ip, prefix) => write!(
                f,
                "{}.{}.{}.{}/{}",
                (ip >> 24) & 0xFF,
                (ip >> 16) & 0xFF,
                (ip >> 8) & 0xFF,
                ip & 0xFF,
                prefix
            ),
            Value::Date(days) => write!(f, "{}", format_civil_date(*days)),
            Value::Time(ms) => {
                let total_secs = ms / 1000;
                write!(
                    f,
                    "{:02}:{:02}:{:02}",
                    total_secs / 3600,
                    (total_secs / 60) % 60,
                    total_secs % 60
                )
            }
            Value::Decimal(v) => write!(f, "{}", format_scaled_i64(*v, 4)),
            Value::EnumValue(i) => write!(f, "enum({})", i),
            Value::Array(elems) => {
                write!(f, "[")?;
                for (i, elem) in elems.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", elem)?;
                }
                write!(f, "]")
            }
            Value::TimestampMs(ms) => {
                let secs = ms / 1000;
                let millis = (ms % 1000).unsigned_abs() as u32;
                let days = (secs / 86400) as i32;
                let day_secs = (secs % 86400) as u32;
                let h = day_secs / 3600;
                let m = (day_secs / 60) % 60;
                let s = day_secs % 60;
                write!(
                    f,
                    "{}T{:02}:{:02}:{:02}.{:03}Z",
                    format_civil_date(days),
                    h,
                    m,
                    s,
                    millis
                )
            }
            Value::Ipv4(ip) => write!(
                f,
                "{}.{}.{}.{}",
                (ip >> 24) & 0xFF,
                (ip >> 16) & 0xFF,
                (ip >> 8) & 0xFF,
                ip & 0xFF
            ),
            Value::Ipv6(bytes) => {
                let addr = std::net::Ipv6Addr::from(*bytes);
                write!(f, "{}", addr)
            }
            Value::Subnet(ip, mask) => {
                let prefix = mask.leading_ones();
                if prefix < 32 && (*mask << prefix) == 0 || prefix == 32 {
                    write!(
                        f,
                        "{}.{}.{}.{}/{}",
                        (ip >> 24) & 0xFF,
                        (ip >> 16) & 0xFF,
                        (ip >> 8) & 0xFF,
                        ip & 0xFF,
                        prefix
                    )
                } else {
                    write!(
                        f,
                        "{}.{}.{}.{}/{}.{}.{}.{}",
                        (ip >> 24) & 0xFF,
                        (ip >> 16) & 0xFF,
                        (ip >> 8) & 0xFF,
                        ip & 0xFF,
                        (mask >> 24) & 0xFF,
                        (mask >> 16) & 0xFF,
                        (mask >> 8) & 0xFF,
                        mask & 0xFF
                    )
                }
            }
            Value::Port(p) => write!(f, "{}", p),
            Value::Latitude(micro) => write!(f, "{:.6}", *micro as f64 / 1_000_000.0),
            Value::Longitude(micro) => write!(f, "{:.6}", *micro as f64 / 1_000_000.0),
            Value::GeoPoint(lat, lon) => write!(
                f,
                "{:.6},{:.6}",
                *lat as f64 / 1_000_000.0,
                *lon as f64 / 1_000_000.0
            ),
            Value::Country2(c) => write!(f, "{}", String::from_utf8_lossy(c)),
            Value::Country3(c) => write!(f, "{}", String::from_utf8_lossy(c)),
            Value::Lang2(c) => write!(f, "{}", String::from_utf8_lossy(c)),
            Value::Lang5(c) => write!(f, "{}", String::from_utf8_lossy(c)),
            Value::Currency(c) => write!(f, "{}", String::from_utf8_lossy(c)),
            Value::AssetCode(code) => write!(f, "{}", code),
            Value::Money {
                asset_code,
                minor_units,
                scale,
            } => write!(
                f,
                "{} {}",
                asset_code,
                format_scaled_i64(*minor_units, *scale)
            ),
            Value::ColorAlpha([r, g, b, a]) => write!(f, "#{:02X}{:02X}{:02X}{:02X}", r, g, b, a),
            Value::BigInt(v) => write!(f, "{}", v),
            Value::KeyRef(c, k) => write!(f, "key_ref:{}:{}", c, k),
            Value::DocRef(c, id) => write!(f, "doc_ref:{}#{}", c, id),
            Value::TableRef(t) => write!(f, "table_ref:{}", t),
            Value::PageRef(p) => write!(f, "page_ref:{}", p),
            Value::Secret(b) => write!(f, "<secret {} bytes>", b.len()),
            Value::Password(_) => write!(f, "***"),
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

/// Convert days since Unix epoch back to YYYY-MM-DD (Howard Hinnant's civil_from_days)
fn format_civil_date(days: i32) -> String {
    let z = days as i64 + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
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
            DataType::Color,
            DataType::Email,
            DataType::Url,
            DataType::Phone,
            DataType::Semver,
            DataType::Cidr,
            DataType::Date,
            DataType::Time,
            DataType::Decimal,
            DataType::Enum,
            DataType::Array,
            DataType::TimestampMs,
            DataType::Ipv4,
            DataType::Ipv6,
            DataType::Subnet,
            DataType::Port,
            DataType::Latitude,
            DataType::Longitude,
            DataType::GeoPoint,
            DataType::Country2,
            DataType::Country3,
            DataType::Lang2,
            DataType::Lang5,
            DataType::Currency,
            DataType::ColorAlpha,
            DataType::BigInt,
        ];

        for dt in types {
            let byte = dt.to_byte();
            let recovered = DataType::from_byte(byte).unwrap();
            assert_eq!(dt, recovered);
        }
    }

    #[test]
    fn test_from_sql_name_uses_shared_alias_mapping() {
        assert_eq!(DataType::from_sql_name("INT8"), Some(DataType::BigInt));
        assert_eq!(DataType::from_sql_name("BIGINT"), Some(DataType::BigInt));
        assert_eq!(
            DataType::from_sql_name("TIMESTAMPTZ"),
            Some(DataType::TimestampMs)
        );
        assert_eq!(DataType::from_sql_name("ROWREF"), Some(DataType::RowRef));
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

    #[test]
    fn test_value_color_roundtrip() {
        let value = Value::Color([0xFF, 0x57, 0x33]);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
        assert_eq!(value.display_string(), "#FF5733");
    }

    #[test]
    fn test_value_email_roundtrip() {
        let value = Value::Email("user@example.com".to_string());
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
    }

    #[test]
    fn test_value_url_roundtrip() {
        let value = Value::Url("https://example.com/path?q=1".to_string());
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
    }

    #[test]
    fn test_value_phone_roundtrip() {
        let value = Value::Phone(5511999887766);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
        assert_eq!(value.display_string(), "+5511999887766");
    }

    #[test]
    fn test_value_semver_roundtrip() {
        let value = Value::Semver(1_000_000 + 23 * 1_000 + 456);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
        assert_eq!(value.display_string(), "1.23.456");
    }

    #[test]
    fn test_value_cidr_roundtrip() {
        // 10.0.0.0/8
        let ip: u32 = 10 << 24;
        let value = Value::Cidr(ip, 8);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
        assert_eq!(value.display_string(), "10.0.0.0/8");
    }

    #[test]
    fn test_value_date_roundtrip() {
        // 2024-01-15 -> some days since epoch
        let value = Value::Date(19738); // approximately 2024-01-15
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
    }

    #[test]
    fn test_value_time_roundtrip() {
        // 14:30:00 -> (14*3600 + 30*60) * 1000 = 52_200_000
        let value = Value::Time(52_200_000);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
        assert_eq!(value.display_string(), "14:30:00");
    }

    #[test]
    fn test_value_decimal_roundtrip() {
        // 123.4567 * 10000 = 1234567
        let value = Value::Decimal(1_234_567);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
        assert_eq!(value.display_string(), "123.4567");
    }

    #[test]
    fn test_value_enum_roundtrip() {
        let value = Value::EnumValue(3);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
        assert_eq!(value.display_string(), "enum(3)");
    }

    #[test]
    fn test_value_array_roundtrip() {
        let value = Value::Array(vec![
            Value::Integer(1),
            Value::Integer(2),
            Value::Integer(3),
        ]);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
    }

    #[test]
    fn test_value_array_nested_roundtrip() {
        let value = Value::Array(vec![
            Value::Text("hello".to_string()),
            Value::Text("world".to_string()),
        ]);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
    }

    #[test]
    fn test_value_date_display_epoch() {
        // Day 0 = 1970-01-01
        let value = Value::Date(0);
        assert_eq!(value.display_string(), "1970-01-01");
    }

    #[test]
    fn test_new_datatype_properties() {
        // Fixed sizes
        assert_eq!(DataType::Color.fixed_size(), Some(3));
        assert_eq!(DataType::Phone.fixed_size(), Some(8));
        assert_eq!(DataType::Semver.fixed_size(), Some(4));
        assert_eq!(DataType::Cidr.fixed_size(), Some(5));
        assert_eq!(DataType::Date.fixed_size(), Some(4));
        assert_eq!(DataType::Time.fixed_size(), Some(4));
        assert_eq!(DataType::Decimal.fixed_size(), Some(8));
        assert_eq!(DataType::Enum.fixed_size(), Some(1));
        assert_eq!(DataType::Email.fixed_size(), None);
        assert_eq!(DataType::Url.fixed_size(), None);
        assert_eq!(DataType::Array.fixed_size(), None);

        // Indexable
        assert!(DataType::Email.is_indexable());
        assert!(DataType::Date.is_indexable());
        assert!(DataType::Decimal.is_indexable());
        assert!(!DataType::Color.is_indexable());
        assert!(!DataType::Array.is_indexable());

        // Orderable
        assert!(DataType::Date.is_orderable());
        assert!(DataType::Time.is_orderable());
        assert!(DataType::Decimal.is_orderable());
        assert!(DataType::Semver.is_orderable());
        assert!(!DataType::Color.is_orderable());
        assert!(!DataType::Email.is_orderable());
    }

    #[test]
    fn test_row_with_new_types() {
        let row = Row::new(vec![
            Value::Color([0xAA, 0xBB, 0xCC]),
            Value::Email("test@example.com".to_string()),
            Value::Phone(1234567890),
            Value::Semver(2_003_001), // 2.3.1
            Value::Date(19738),
            Value::EnumValue(1),
        ]);

        let bytes = row.to_bytes();
        let (recovered, size) = Row::from_bytes(&bytes).unwrap();
        assert_eq!(row, recovered);
        assert_eq!(size, bytes.len());
    }

    // --- New type serialization roundtrips ---

    #[test]
    fn test_value_timestamp_ms_roundtrip() {
        let value = Value::TimestampMs(1710510600123);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
    }

    #[test]
    fn test_value_ipv4_roundtrip() {
        let ip = (192u32 << 24) | (168 << 16) | (1 << 8) | 1;
        let value = Value::Ipv4(ip);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
        assert_eq!(value.display_string(), "192.168.1.1");
    }

    #[test]
    fn test_value_ipv6_roundtrip() {
        let mut octets = [0u8; 16];
        octets[15] = 1;
        let value = Value::Ipv6(octets);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
        assert_eq!(value.display_string(), "::1");
    }

    #[test]
    fn test_value_subnet_roundtrip() {
        let ip = 10u32 << 24;
        let mask = !0u32 << 16; // /16
        let value = Value::Subnet(ip, mask);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
        assert_eq!(value.display_string(), "10.0.0.0/16");
    }

    #[test]
    fn test_value_port_roundtrip() {
        let value = Value::Port(8080);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
        assert_eq!(value.display_string(), "8080");
    }

    #[test]
    fn test_value_latitude_roundtrip() {
        let value = Value::Latitude(-23550520);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
        assert_eq!(value.display_string(), "-23.550520");
    }

    #[test]
    fn test_value_longitude_roundtrip() {
        let value = Value::Longitude(-46633308);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
        assert_eq!(value.display_string(), "-46.633308");
    }

    #[test]
    fn test_value_geopoint_roundtrip() {
        let value = Value::GeoPoint(-23550520, -46633308);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
    }

    #[test]
    fn test_value_country2_roundtrip() {
        let value = Value::Country2([b'B', b'R']);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
        assert_eq!(value.display_string(), "BR");
    }

    #[test]
    fn test_value_country3_roundtrip() {
        let value = Value::Country3([b'B', b'R', b'A']);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
        assert_eq!(value.display_string(), "BRA");
    }

    #[test]
    fn test_value_lang2_roundtrip() {
        let value = Value::Lang2([b'p', b't']);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
        assert_eq!(value.display_string(), "pt");
    }

    #[test]
    fn test_value_lang5_roundtrip() {
        let value = Value::Lang5([b'p', b't', b'-', b'B', b'R']);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
        assert_eq!(value.display_string(), "pt-BR");
    }

    #[test]
    fn test_value_currency_roundtrip() {
        let value = Value::Currency([b'U', b'S', b'D']);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
        assert_eq!(value.display_string(), "USD");
    }

    #[test]
    fn test_value_color_alpha_roundtrip() {
        let value = Value::ColorAlpha([0xFF, 0x57, 0x33, 0x80]);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
        assert_eq!(value.display_string(), "#FF573380");
    }

    #[test]
    fn test_value_bigint_roundtrip() {
        let value = Value::BigInt(i64::MAX);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
    }

    #[test]
    fn test_new_datatype_roundtrip() {
        let types = [
            DataType::TimestampMs,
            DataType::Ipv4,
            DataType::Ipv6,
            DataType::Subnet,
            DataType::Port,
            DataType::Latitude,
            DataType::Longitude,
            DataType::GeoPoint,
            DataType::Country2,
            DataType::Country3,
            DataType::Lang2,
            DataType::Lang5,
            DataType::Currency,
            DataType::ColorAlpha,
            DataType::BigInt,
        ];

        for dt in types {
            let byte = dt.to_byte();
            let recovered = DataType::from_byte(byte).unwrap();
            assert_eq!(dt, recovered);
        }
    }

    #[test]
    fn test_rich_type_datatype_properties() {
        // Fixed sizes
        assert_eq!(DataType::TimestampMs.fixed_size(), Some(8));
        assert_eq!(DataType::Ipv4.fixed_size(), Some(4));
        assert_eq!(DataType::Ipv6.fixed_size(), Some(16));
        assert_eq!(DataType::Subnet.fixed_size(), Some(8));
        assert_eq!(DataType::Port.fixed_size(), Some(2));
        assert_eq!(DataType::Latitude.fixed_size(), Some(4));
        assert_eq!(DataType::Longitude.fixed_size(), Some(4));
        assert_eq!(DataType::GeoPoint.fixed_size(), Some(8));
        assert_eq!(DataType::Country2.fixed_size(), Some(2));
        assert_eq!(DataType::Country3.fixed_size(), Some(3));
        assert_eq!(DataType::Lang2.fixed_size(), Some(2));
        assert_eq!(DataType::Lang5.fixed_size(), Some(5));
        assert_eq!(DataType::Currency.fixed_size(), Some(3));
        assert_eq!(DataType::ColorAlpha.fixed_size(), Some(4));
        assert_eq!(DataType::BigInt.fixed_size(), Some(8));

        // Indexable
        assert!(DataType::TimestampMs.is_indexable());
        assert!(DataType::Ipv4.is_indexable());
        assert!(DataType::Ipv6.is_indexable());
        assert!(DataType::Port.is_indexable());
        assert!(DataType::Country2.is_indexable());
        assert!(DataType::Currency.is_indexable());
        assert!(DataType::BigInt.is_indexable());
        assert!(!DataType::Subnet.is_indexable());
        assert!(!DataType::ColorAlpha.is_indexable());

        // Orderable
        assert!(DataType::TimestampMs.is_orderable());
        assert!(DataType::Port.is_orderable());
        assert!(DataType::Latitude.is_orderable());
        assert!(DataType::Longitude.is_orderable());
        assert!(DataType::BigInt.is_orderable());
        assert!(!DataType::Country2.is_orderable());
        assert!(!DataType::Ipv4.is_orderable());
    }

    #[test]
    fn test_row_with_all_new_types() {
        let row = Row::new(vec![
            Value::TimestampMs(1710510600123),
            Value::Ipv4((10u32 << 24) | 1),
            Value::Ipv6([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]),
            Value::Subnet(10u32 << 24, !0u32 << 16),
            Value::Port(443),
            Value::Latitude(-23550520),
            Value::Longitude(-46633308),
            Value::GeoPoint(-23550520, -46633308),
            Value::Country2([b'B', b'R']),
            Value::Country3([b'B', b'R', b'A']),
            Value::Lang2([b'p', b't']),
            Value::Lang5([b'p', b't', b'-', b'B', b'R']),
            Value::Currency([b'U', b'S', b'D']),
            Value::ColorAlpha([0xFF, 0x57, 0x33, 0x80]),
            Value::BigInt(i64::MAX),
        ]);

        let bytes = row.to_bytes();
        let (recovered, size) = Row::from_bytes(&bytes).unwrap();
        assert_eq!(row, recovered);
        assert_eq!(size, bytes.len());
    }
}
