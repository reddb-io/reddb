//! Persisted SQL table-definition payload codec (`RTBL`).
//!
//! The schema engine owns column types, index/constraint semantics, and
//! validation. This module owns only the durable byte layout of a serialized
//! table definition. Type discriminants (`data_type`, `element_type`,
//! `index_type`, `constraint_type`) are carried as opaque `u8` bytes: the
//! engine maps them to/from its `DataType` / `IndexType` / `ConstraintType`
//! enums and is responsible for rejecting unknown discriminants.
//!
//! `reddb-file` already owns this table definition as the opaque
//! `table_def_hex` field of `PhysicalCollectionContract`; this codec gives that
//! hex blob a structured home.
//!
//! Strings are varint-length-prefixed UTF-8; counts are LEB128 varints; fixed
//! integers are little-endian. DO NOT change magic/order/width — these bytes
//! live in existing `.rdb` files.

/// Magic prefix for a serialized table definition.
pub const TABLE_DEF_MAGIC: [u8; 4] = *b"RTBL";

/// A decoded column definition. Type bytes are opaque to this crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnDefFrame {
    pub name: String,
    pub data_type: u8,
    pub nullable: bool,
    pub default: Option<Vec<u8>>,
    pub vector_dim: Option<u32>,
    pub compress: bool,
    pub enum_variants: Vec<String>,
    pub decimal_precision: u8,
    pub element_type: Option<u8>,
    /// Key/value metadata, in on-disk order.
    pub metadata: Vec<(String, String)>,
}

/// A decoded index definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexDefFrame {
    pub name: String,
    pub index_type: u8,
    pub unique: bool,
    pub columns: Vec<String>,
}

/// A decoded constraint definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstraintFrame {
    pub name: String,
    pub constraint_type: u8,
    pub columns: Vec<String>,
    pub ref_table: Option<String>,
    pub ref_columns: Option<Vec<String>>,
}

/// A decoded table definition (`RTBL`) payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableDefFrame {
    pub name: String,
    pub version: u32,
    pub created_at: u64,
    pub updated_at: u64,
    pub columns: Vec<ColumnDefFrame>,
    pub primary_key: Vec<String>,
    pub indexes: Vec<IndexDefFrame>,
    pub constraints: Vec<ConstraintFrame>,
}

/// Errors decoding a table-definition payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableDefFrameError {
    TruncatedData,
    InvalidMagic,
    VarintOverflow,
}

impl std::fmt::Display for TableDefFrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TruncatedData => write!(f, "truncated data"),
            Self::InvalidMagic => write!(f, "invalid magic bytes"),
            Self::VarintOverflow => write!(f, "varint overflow"),
        }
    }
}

impl std::error::Error for TableDefFrameError {}

// ============================================================================
// Encode
// ============================================================================

/// Serialize a table-definition payload to bytes.
pub fn encode_table_def_frame(frame: &TableDefFrame) -> Vec<u8> {
    let mut buf = Vec::new();

    buf.extend_from_slice(&TABLE_DEF_MAGIC);
    buf.extend_from_slice(&frame.version.to_le_bytes());
    write_string(&mut buf, &frame.name);
    buf.extend_from_slice(&frame.created_at.to_le_bytes());
    buf.extend_from_slice(&frame.updated_at.to_le_bytes());

    write_varint(&mut buf, frame.columns.len() as u64);
    for col in &frame.columns {
        write_column(&mut buf, col);
    }

    write_varint(&mut buf, frame.primary_key.len() as u64);
    for pk in &frame.primary_key {
        write_string(&mut buf, pk);
    }

    write_varint(&mut buf, frame.indexes.len() as u64);
    for idx in &frame.indexes {
        write_index(&mut buf, idx);
    }

    write_varint(&mut buf, frame.constraints.len() as u64);
    for constraint in &frame.constraints {
        write_constraint(&mut buf, constraint);
    }

    buf
}

fn write_column(buf: &mut Vec<u8>, col: &ColumnDefFrame) {
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

fn write_index(buf: &mut Vec<u8>, idx: &IndexDefFrame) {
    write_string(buf, &idx.name);
    buf.push(idx.index_type);
    buf.push(if idx.unique { 1 } else { 0 });
    write_varint(buf, idx.columns.len() as u64);
    for col in &idx.columns {
        write_string(buf, col);
    }
}

fn write_constraint(buf: &mut Vec<u8>, constraint: &ConstraintFrame) {
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

// ============================================================================
// Decode
// ============================================================================

/// Deserialize a table-definition payload from bytes.
pub fn decode_table_def_frame(data: &[u8]) -> Result<TableDefFrame, TableDefFrameError> {
    if data.len() < 4 {
        return Err(TableDefFrameError::TruncatedData);
    }
    if data[0..4] != TABLE_DEF_MAGIC {
        return Err(TableDefFrameError::InvalidMagic);
    }

    let mut offset = 4;

    if data.len() < offset + 4 {
        return Err(TableDefFrameError::TruncatedData);
    }
    let version = u32::from_le_bytes(data[offset..offset + 4].try_into().expect("u32 checked"));
    offset += 4;

    let (name, name_len) = read_string(&data[offset..])?;
    offset += name_len;

    if data.len() < offset + 16 {
        return Err(TableDefFrameError::TruncatedData);
    }
    let created_at = u64::from_le_bytes(data[offset..offset + 8].try_into().expect("u64 checked"));
    offset += 8;
    let updated_at = u64::from_le_bytes(data[offset..offset + 8].try_into().expect("u64 checked"));
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

    Ok(TableDefFrame {
        name,
        version,
        created_at,
        updated_at,
        columns,
        primary_key,
        indexes,
        constraints,
    })
}

fn read_column(data: &[u8]) -> Result<(ColumnDefFrame, usize), TableDefFrameError> {
    let mut offset = 0;

    let (name, name_len) = read_string(&data[offset..])?;
    offset += name_len;

    if data.len() < offset + 2 {
        return Err(TableDefFrameError::TruncatedData);
    }
    let data_type = data[offset];
    offset += 1;
    let nullable = data[offset] != 0;
    offset += 1;

    if data.len() < offset + 1 {
        return Err(TableDefFrameError::TruncatedData);
    }
    let has_default = data[offset] != 0;
    offset += 1;
    let default = if has_default {
        let (len, varint_len) = read_varint(&data[offset..])?;
        offset += varint_len;
        if data.len() < offset + len as usize {
            return Err(TableDefFrameError::TruncatedData);
        }
        let default_data = data[offset..offset + len as usize].to_vec();
        offset += len as usize;
        Some(default_data)
    } else {
        None
    };

    if data.len() < offset + 1 {
        return Err(TableDefFrameError::TruncatedData);
    }
    let has_vector_dim = data[offset] != 0;
    offset += 1;
    let vector_dim = if has_vector_dim {
        if data.len() < offset + 4 {
            return Err(TableDefFrameError::TruncatedData);
        }
        let dim = u32::from_le_bytes(data[offset..offset + 4].try_into().expect("u32 checked"));
        offset += 4;
        Some(dim)
    } else {
        None
    };

    if data.len() < offset + 1 {
        return Err(TableDefFrameError::TruncatedData);
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
        return Err(TableDefFrameError::TruncatedData);
    }
    let decimal_precision = data[offset];
    offset += 1;

    if data.len() < offset + 1 {
        return Err(TableDefFrameError::TruncatedData);
    }
    let has_element_type = data[offset] != 0;
    offset += 1;
    let element_type = if has_element_type {
        if data.len() < offset + 1 {
            return Err(TableDefFrameError::TruncatedData);
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
        ColumnDefFrame {
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

fn read_index(data: &[u8]) -> Result<(IndexDefFrame, usize), TableDefFrameError> {
    let mut offset = 0;

    let (name, name_len) = read_string(&data[offset..])?;
    offset += name_len;

    if data.len() < offset + 2 {
        return Err(TableDefFrameError::TruncatedData);
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
        IndexDefFrame {
            name,
            index_type,
            unique,
            columns,
        },
        offset,
    ))
}

fn read_constraint(data: &[u8]) -> Result<(ConstraintFrame, usize), TableDefFrameError> {
    let mut offset = 0;

    let (name, name_len) = read_string(&data[offset..])?;
    offset += name_len;

    if data.len() < offset + 1 {
        return Err(TableDefFrameError::TruncatedData);
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
        return Err(TableDefFrameError::TruncatedData);
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
        ConstraintFrame {
            name,
            constraint_type,
            columns,
            ref_table,
            ref_columns,
        },
        offset,
    ))
}

// ============================================================================
// Varint + string primitives (LEB128, varint-prefixed UTF-8)
// ============================================================================

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

fn read_varint(data: &[u8]) -> Result<(u64, usize), TableDefFrameError> {
    let mut result: u64 = 0;
    let mut shift = 0;
    let mut offset = 0;

    loop {
        if offset >= data.len() {
            return Err(TableDefFrameError::TruncatedData);
        }
        let byte = data[offset];
        offset += 1;

        if shift >= 64 {
            return Err(TableDefFrameError::VarintOverflow);
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

fn read_string(data: &[u8]) -> Result<(String, usize), TableDefFrameError> {
    let (len, varint_len) = read_varint(data)?;
    let offset = varint_len;
    if data.len() < offset + len as usize {
        return Err(TableDefFrameError::TruncatedData);
    }
    let s = String::from_utf8(data[offset..offset + len as usize].to_vec())
        .map_err(|_| TableDefFrameError::TruncatedData)?;
    Ok((s, offset + len as usize))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_frame() -> TableDefFrame {
        TableDefFrame {
            name: "embeddings".into(),
            version: 1,
            created_at: 1_700_000_000,
            updated_at: 1_700_000_500,
            columns: vec![
                ColumnDefFrame {
                    name: "id".into(),
                    data_type: 2,
                    nullable: false,
                    default: None,
                    vector_dim: None,
                    compress: false,
                    enum_variants: vec![],
                    decimal_precision: 4,
                    element_type: None,
                    metadata: vec![],
                },
                ColumnDefFrame {
                    name: "embedding".into(),
                    data_type: 11,
                    nullable: false,
                    default: Some(vec![1, 2, 3]),
                    vector_dim: Some(384),
                    compress: true,
                    enum_variants: vec!["a".into(), "b".into()],
                    decimal_precision: 6,
                    element_type: Some(3),
                    metadata: vec![("unit".into(), "f32".into())],
                },
            ],
            primary_key: vec!["id".into()],
            indexes: vec![IndexDefFrame {
                name: "idx_vec".into(),
                index_type: 4,
                unique: false,
                columns: vec!["embedding".into()],
            }],
            constraints: vec![
                ConstraintFrame {
                    name: "fk".into(),
                    constraint_type: 3,
                    columns: vec!["id".into()],
                    ref_table: Some("other".into()),
                    ref_columns: Some(vec!["oid".into()]),
                },
                ConstraintFrame {
                    name: "nn".into(),
                    constraint_type: 5,
                    columns: vec!["id".into()],
                    ref_table: None,
                    ref_columns: None,
                },
            ],
        }
    }

    #[test]
    fn table_def_frame_round_trips() {
        let frame = sample_frame();
        let encoded = encode_table_def_frame(&frame);
        let decoded = decode_table_def_frame(&encoded).unwrap();
        assert_eq!(decoded, frame);
        assert_eq!(encode_table_def_frame(&decoded), encoded);
    }

    #[test]
    fn table_def_frame_pins_magic_and_version() {
        let encoded = encode_table_def_frame(&sample_frame());
        assert_eq!(&encoded[0..4], b"RTBL");
        assert_eq!(&encoded[4..8], &1u32.to_le_bytes());
    }

    #[test]
    fn table_def_frame_rejects_bad_input() {
        assert_eq!(
            decode_table_def_frame(&[0u8; 2]),
            Err(TableDefFrameError::TruncatedData)
        );
        let mut bad = encode_table_def_frame(&sample_frame());
        bad[0] = b'X';
        assert_eq!(
            decode_table_def_frame(&bad),
            Err(TableDefFrameError::InvalidMagic)
        );
        let encoded = encode_table_def_frame(&sample_frame());
        assert_eq!(
            decode_table_def_frame(&encoded[..encoded.len() - 1]),
            Err(TableDefFrameError::TruncatedData)
        );
    }
}
