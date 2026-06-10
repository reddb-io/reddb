//! On-disk codec for the `RTBL` table-definition payload.
//!
//! `reddb-file` already owns the opaque `table_def_hex` field of the physical
//! collection contract; this module makes it the authority for the inner byte
//! layout too (ADR 0046). The codec operates on a plain [`TableDefLayout`] whose
//! type/index/constraint discriminants are raw bytes — the server engine owns
//! the SQL-level `DataType`/`IndexType`/`ConstraintType` enums and maps them to
//! and from these bytes. The magic, version, LEB128 varint + length-prefixed
//! string framing, and field ordering all live here.

/// Magic prefixing a persisted table definition.
pub const TABLE_DEF_MAGIC: &[u8; 4] = b"RTBL";

/// Plain, engine-agnostic view of a persisted column definition.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnLayout {
    /// Column name.
    pub name: String,
    /// `DataType` discriminant byte.
    pub data_type: u8,
    /// Whether NULL is allowed.
    pub nullable: bool,
    /// Serialized default value, if any.
    pub default: Option<Vec<u8>>,
    /// Vector dimension, if any.
    pub vector_dim: Option<u32>,
    /// Per-column compression flag.
    pub compress: bool,
    /// Enum variant labels (for enum-typed columns).
    pub enum_variants: Vec<String>,
    /// Decimal precision.
    pub decimal_precision: u8,
    /// Array element `DataType` discriminant byte, if any.
    pub element_type: Option<u8>,
    /// Column metadata `(key, value)` pairs in persistence order.
    pub metadata: Vec<(String, String)>,
}

/// Plain, engine-agnostic view of a persisted index definition.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexLayout {
    /// Index name.
    pub name: String,
    /// `IndexType` discriminant byte.
    pub index_type: u8,
    /// Whether the index is unique.
    pub unique: bool,
    /// Indexed column names in order.
    pub columns: Vec<String>,
}

/// Plain, engine-agnostic view of a persisted constraint definition.
#[derive(Debug, Clone, PartialEq)]
pub struct ConstraintLayout {
    /// Constraint name.
    pub name: String,
    /// `ConstraintType` discriminant byte.
    pub constraint_type: u8,
    /// Constrained column names.
    pub columns: Vec<String>,
    /// Referenced table (foreign keys).
    pub ref_table: Option<String>,
    /// Referenced columns (foreign keys).
    pub ref_columns: Option<Vec<String>>,
}

/// Plain, engine-agnostic view of a persisted table definition.
#[derive(Debug, Clone, PartialEq)]
pub struct TableDefLayout {
    /// Schema version field.
    pub version: u32,
    /// Table name.
    pub name: String,
    /// Creation timestamp.
    pub created_at: u64,
    /// Last-update timestamp.
    pub updated_at: u64,
    /// Columns in declaration order.
    pub columns: Vec<ColumnLayout>,
    /// Primary-key column names.
    pub primary_key: Vec<String>,
    /// Indexes.
    pub indexes: Vec<IndexLayout>,
    /// Constraints.
    pub constraints: Vec<ConstraintLayout>,
}

/// Errors raised while decoding a persisted table definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableDefCodecError {
    /// The payload was shorter than required.
    TruncatedData,
    /// The leading magic was not `RTBL`.
    InvalidMagic,
    /// A varint exceeded 64 bits.
    VarintOverflow,
    /// A length-prefixed string contained invalid UTF-8.
    InvalidUtf8,
}

impl std::fmt::Display for TableDefCodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TableDefCodecError::TruncatedData => write!(f, "truncated data"),
            TableDefCodecError::InvalidMagic => write!(f, "invalid magic bytes"),
            TableDefCodecError::VarintOverflow => write!(f, "varint overflow"),
            TableDefCodecError::InvalidUtf8 => write!(f, "invalid utf-8"),
        }
    }
}

impl std::error::Error for TableDefCodecError {}

// ---------------------------------------------------------------------------
// Encode
// ---------------------------------------------------------------------------

/// Encode a table definition (`RTBL`), byte-faithful to the legacy server.
pub fn encode_table_def(layout: &TableDefLayout) -> Vec<u8> {
    let mut buf = Vec::new();

    buf.extend_from_slice(TABLE_DEF_MAGIC);
    buf.extend_from_slice(&layout.version.to_le_bytes());
    write_string(&mut buf, &layout.name);
    buf.extend_from_slice(&layout.created_at.to_le_bytes());
    buf.extend_from_slice(&layout.updated_at.to_le_bytes());

    write_varint(&mut buf, layout.columns.len() as u64);
    for col in &layout.columns {
        write_column(&mut buf, col);
    }

    write_varint(&mut buf, layout.primary_key.len() as u64);
    for pk in &layout.primary_key {
        write_string(&mut buf, pk);
    }

    write_varint(&mut buf, layout.indexes.len() as u64);
    for idx in &layout.indexes {
        write_index(&mut buf, idx);
    }

    write_varint(&mut buf, layout.constraints.len() as u64);
    for constraint in &layout.constraints {
        write_constraint(&mut buf, constraint);
    }

    buf
}

fn write_column(buf: &mut Vec<u8>, col: &ColumnLayout) {
    write_string(buf, &col.name);
    buf.push(col.data_type);
    buf.push(if col.nullable { 1 } else { 0 });

    if let Some(ref default) = col.default {
        buf.push(1);
        write_varint(buf, default.len() as u64);
        buf.extend_from_slice(default);
    } else {
        buf.push(0);
    }

    if let Some(dim) = col.vector_dim {
        buf.push(1);
        buf.extend_from_slice(&dim.to_le_bytes());
    } else {
        buf.push(0);
    }

    buf.push(if col.compress { 1 } else { 0 });

    write_varint(buf, col.enum_variants.len() as u64);
    for variant in &col.enum_variants {
        write_string(buf, variant);
    }

    buf.push(col.decimal_precision);

    if let Some(et) = col.element_type {
        buf.push(1);
        buf.push(et);
    } else {
        buf.push(0);
    }

    write_varint(buf, col.metadata.len() as u64);
    for (k, v) in &col.metadata {
        write_string(buf, k);
        write_string(buf, v);
    }
}

fn write_index(buf: &mut Vec<u8>, idx: &IndexLayout) {
    write_string(buf, &idx.name);
    buf.push(idx.index_type);
    buf.push(if idx.unique { 1 } else { 0 });
    write_varint(buf, idx.columns.len() as u64);
    for col in &idx.columns {
        write_string(buf, col);
    }
}

fn write_constraint(buf: &mut Vec<u8>, constraint: &ConstraintLayout) {
    write_string(buf, &constraint.name);
    buf.push(constraint.constraint_type);

    write_varint(buf, constraint.columns.len() as u64);
    for col in &constraint.columns {
        write_string(buf, col);
    }

    if let Some(ref table) = constraint.ref_table {
        buf.push(1);
        write_string(buf, table);
        if let Some(ref cols) = constraint.ref_columns {
            write_varint(buf, cols.len() as u64);
            for col in cols {
                write_string(buf, col);
            }
        } else {
            write_varint(buf, 0);
        }
    } else {
        buf.push(0);
    }
}

// ---------------------------------------------------------------------------
// Decode
// ---------------------------------------------------------------------------

/// Decode a table definition (`RTBL`) produced by [`encode_table_def`] or the
/// legacy server `to_bytes`.
pub fn decode_table_def(data: &[u8]) -> Result<TableDefLayout, TableDefCodecError> {
    if data.len() < 4 {
        return Err(TableDefCodecError::TruncatedData);
    }
    if &data[0..4] != TABLE_DEF_MAGIC {
        return Err(TableDefCodecError::InvalidMagic);
    }

    let mut offset = 4;

    if data.len() < offset + 4 {
        return Err(TableDefCodecError::TruncatedData);
    }
    let version = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
    offset += 4;

    let (name, name_len) = read_string(&data[offset..])?;
    offset += name_len;

    if data.len() < offset + 16 {
        return Err(TableDefCodecError::TruncatedData);
    }
    let created_at = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
    offset += 8;
    let updated_at = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
    offset += 8;

    let (col_count, varint_len) = read_varint(&data[offset..])?;
    offset += varint_len;
    let mut columns = Vec::with_capacity(col_count as usize);
    for _ in 0..col_count {
        let (col, col_len) = read_column(&data[offset..])?;
        offset += col_len;
        columns.push(col);
    }

    let (pk_count, varint_len) = read_varint(&data[offset..])?;
    offset += varint_len;
    let mut primary_key = Vec::with_capacity(pk_count as usize);
    for _ in 0..pk_count {
        let (pk, pk_len) = read_string(&data[offset..])?;
        offset += pk_len;
        primary_key.push(pk);
    }

    let (idx_count, varint_len) = read_varint(&data[offset..])?;
    offset += varint_len;
    let mut indexes = Vec::with_capacity(idx_count as usize);
    for _ in 0..idx_count {
        let (idx, idx_len) = read_index(&data[offset..])?;
        offset += idx_len;
        indexes.push(idx);
    }

    let (constraint_count, varint_len) = read_varint(&data[offset..])?;
    offset += varint_len;
    let mut constraints = Vec::with_capacity(constraint_count as usize);
    for _ in 0..constraint_count {
        let (constraint, constraint_len) = read_constraint(&data[offset..])?;
        offset += constraint_len;
        constraints.push(constraint);
    }

    Ok(TableDefLayout {
        version,
        name,
        created_at,
        updated_at,
        columns,
        primary_key,
        indexes,
        constraints,
    })
}

fn read_column(data: &[u8]) -> Result<(ColumnLayout, usize), TableDefCodecError> {
    let mut offset = 0;

    let (name, name_len) = read_string(&data[offset..])?;
    offset += name_len;

    if data.len() < offset + 2 {
        return Err(TableDefCodecError::TruncatedData);
    }
    let data_type = data[offset];
    offset += 1;
    let nullable = data[offset] != 0;
    offset += 1;

    if data.len() < offset + 1 {
        return Err(TableDefCodecError::TruncatedData);
    }
    let has_default = data[offset] != 0;
    offset += 1;
    let default = if has_default {
        let (len, varint_len) = read_varint(&data[offset..])?;
        offset += varint_len;
        if data.len() < offset + len as usize {
            return Err(TableDefCodecError::TruncatedData);
        }
        let default_data = data[offset..offset + len as usize].to_vec();
        offset += len as usize;
        Some(default_data)
    } else {
        None
    };

    if data.len() < offset + 1 {
        return Err(TableDefCodecError::TruncatedData);
    }
    let has_vector_dim = data[offset] != 0;
    offset += 1;
    let vector_dim = if has_vector_dim {
        if data.len() < offset + 4 {
            return Err(TableDefCodecError::TruncatedData);
        }
        let dim = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
        offset += 4;
        Some(dim)
    } else {
        None
    };

    if data.len() < offset + 1 {
        return Err(TableDefCodecError::TruncatedData);
    }
    let compress = data[offset] != 0;
    offset += 1;

    let (variant_count, varint_len) = read_varint(&data[offset..])?;
    offset += varint_len;
    let mut enum_variants = Vec::with_capacity(variant_count as usize);
    for _ in 0..variant_count {
        let (variant, variant_len) = read_string(&data[offset..])?;
        offset += variant_len;
        enum_variants.push(variant);
    }

    if data.len() < offset + 1 {
        return Err(TableDefCodecError::TruncatedData);
    }
    let decimal_precision = data[offset];
    offset += 1;

    if data.len() < offset + 1 {
        return Err(TableDefCodecError::TruncatedData);
    }
    let has_element_type = data[offset] != 0;
    offset += 1;
    let element_type = if has_element_type {
        if data.len() < offset + 1 {
            return Err(TableDefCodecError::TruncatedData);
        }
        let et = data[offset];
        offset += 1;
        Some(et)
    } else {
        None
    };

    let (meta_count, varint_len) = read_varint(&data[offset..])?;
    offset += varint_len;
    let mut metadata = Vec::with_capacity(meta_count as usize);
    for _ in 0..meta_count {
        let (k, k_len) = read_string(&data[offset..])?;
        offset += k_len;
        let (v, v_len) = read_string(&data[offset..])?;
        offset += v_len;
        metadata.push((k, v));
    }

    Ok((
        ColumnLayout {
            name,
            data_type,
            nullable,
            default,
            vector_dim,
            compress,
            enum_variants,
            decimal_precision,
            element_type,
            metadata,
        },
        offset,
    ))
}

fn read_index(data: &[u8]) -> Result<(IndexLayout, usize), TableDefCodecError> {
    let mut offset = 0;

    let (name, name_len) = read_string(&data[offset..])?;
    offset += name_len;

    if data.len() < offset + 2 {
        return Err(TableDefCodecError::TruncatedData);
    }
    let index_type = data[offset];
    offset += 1;
    let unique = data[offset] != 0;
    offset += 1;

    let (col_count, varint_len) = read_varint(&data[offset..])?;
    offset += varint_len;
    let mut columns = Vec::with_capacity(col_count as usize);
    for _ in 0..col_count {
        let (col, col_len) = read_string(&data[offset..])?;
        offset += col_len;
        columns.push(col);
    }

    Ok((
        IndexLayout {
            name,
            index_type,
            unique,
            columns,
        },
        offset,
    ))
}

fn read_constraint(data: &[u8]) -> Result<(ConstraintLayout, usize), TableDefCodecError> {
    let mut offset = 0;

    let (name, name_len) = read_string(&data[offset..])?;
    offset += name_len;

    if data.len() < offset + 1 {
        return Err(TableDefCodecError::TruncatedData);
    }
    let constraint_type = data[offset];
    offset += 1;

    let (col_count, varint_len) = read_varint(&data[offset..])?;
    offset += varint_len;
    let mut columns = Vec::with_capacity(col_count as usize);
    for _ in 0..col_count {
        let (col, col_len) = read_string(&data[offset..])?;
        offset += col_len;
        columns.push(col);
    }

    if data.len() < offset + 1 {
        return Err(TableDefCodecError::TruncatedData);
    }
    let has_ref = data[offset] != 0;
    offset += 1;

    let (ref_table, ref_columns) = if has_ref {
        let (table, table_len) = read_string(&data[offset..])?;
        offset += table_len;

        let (ref_col_count, varint_len) = read_varint(&data[offset..])?;
        offset += varint_len;

        let mut ref_cols = Vec::with_capacity(ref_col_count as usize);
        for _ in 0..ref_col_count {
            let (col, col_len) = read_string(&data[offset..])?;
            offset += col_len;
            ref_cols.push(col);
        }

        (Some(table), Some(ref_cols))
    } else {
        (None, None)
    };

    Ok((
        ConstraintLayout {
            name,
            constraint_type,
            columns,
            ref_table,
            ref_columns,
        },
        offset,
    ))
}

// ---------------------------------------------------------------------------
// LEB128 varint + length-prefixed string framing
// ---------------------------------------------------------------------------

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

fn read_varint(data: &[u8]) -> Result<(u64, usize), TableDefCodecError> {
    let mut result: u64 = 0;
    let mut shift = 0;
    let mut offset = 0;

    loop {
        if offset >= data.len() {
            return Err(TableDefCodecError::TruncatedData);
        }
        let byte = data[offset];
        offset += 1;

        if shift >= 64 {
            return Err(TableDefCodecError::VarintOverflow);
        }

        result |= ((byte & 0x7F) as u64) << shift;
        shift += 7;

        if byte & 0x80 == 0 {
            break;
        }
    }

    Ok((result, offset))
}

fn write_string(buf: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    write_varint(buf, bytes.len() as u64);
    buf.extend_from_slice(bytes);
}

fn read_string(data: &[u8]) -> Result<(String, usize), TableDefCodecError> {
    let (len, varint_len) = read_varint(data)?;
    let offset = varint_len;
    if data.len() < offset + len as usize {
        return Err(TableDefCodecError::TruncatedData);
    }
    let s = String::from_utf8(data[offset..offset + len as usize].to_vec())
        .map_err(|_| TableDefCodecError::InvalidUtf8)?;
    Ok((s, offset + len as usize))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> TableDefLayout {
        TableDefLayout {
            version: 1,
            name: "hosts".to_string(),
            created_at: 0x0102_0304_0506_0708,
            updated_at: 0x1112_1314_1516_1718,
            columns: vec![
                ColumnLayout {
                    name: "id".to_string(),
                    data_type: 2,
                    nullable: false,
                    default: None,
                    vector_dim: None,
                    compress: false,
                    enum_variants: Vec::new(),
                    decimal_precision: 4,
                    element_type: None,
                    metadata: vec![("desc".to_string(), "primary".to_string())],
                },
                ColumnLayout {
                    name: "fingerprint".to_string(),
                    data_type: 11,
                    nullable: true,
                    default: Some(vec![1, 2, 3]),
                    vector_dim: Some(128),
                    compress: true,
                    enum_variants: vec!["a".to_string(), "b".to_string()],
                    decimal_precision: 6,
                    element_type: Some(4),
                    metadata: Vec::new(),
                },
            ],
            primary_key: vec!["id".to_string()],
            indexes: vec![IndexLayout {
                name: "idx_fp".to_string(),
                index_type: 3,
                unique: true,
                columns: vec!["fingerprint".to_string()],
            }],
            constraints: vec![ConstraintLayout {
                name: "fk_host".to_string(),
                constraint_type: 3,
                columns: vec!["host_id".to_string()],
                ref_table: Some("hosts".to_string()),
                ref_columns: Some(vec!["id".to_string()]),
            }],
        }
    }

    #[test]
    fn round_trip_preserves_layout() {
        let layout = sample();
        let bytes = encode_table_def(&layout);
        let decoded = decode_table_def(&bytes).expect("decode");
        assert_eq!(decoded, layout);
    }

    #[test]
    fn fixture_bytes_are_byte_identical() {
        let layout = sample();
        let bytes = encode_table_def(&layout);
        assert_eq!(&bytes[0..4], b"RTBL", "magic must lead the payload");
        assert_eq!(&bytes[4..8], &1u32.to_le_bytes(), "version field");
        // name string: varint length (5) + "hosts"
        assert_eq!(bytes[8], 5);
        assert_eq!(&bytes[9..14], b"hosts");
        // created_at u64 little-endian follows the name.
        assert_eq!(&bytes[14..22], &0x0102_0304_0506_0708u64.to_le_bytes());
    }

    #[test]
    fn rejects_short_and_bad_magic() {
        assert_eq!(
            decode_table_def(&[0u8; 2]),
            Err(TableDefCodecError::TruncatedData)
        );
        let mut bytes = encode_table_def(&sample());
        bytes[0] = b'X';
        assert_eq!(
            decode_table_def(&bytes),
            Err(TableDefCodecError::InvalidMagic)
        );
    }
}
