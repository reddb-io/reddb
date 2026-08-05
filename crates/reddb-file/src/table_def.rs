//! Persisted SQL table-definition payload codec (`RTBL`).
//!
//! [`reddb_types`] owns the logical table vocabulary and validation. This
//! module owns only the durable byte layout of a serialized table definition.
//!
//! `reddb-file` already owns this table definition as the opaque
//! `table_def_hex` field of `PhysicalCollectionContract`; this codec gives that
//! hex blob a structured home.
//!
//! Strings are varint-length-prefixed UTF-8; counts are LEB128 varints; fixed
//! integers are little-endian. DO NOT change magic/order/width — these bytes
//! live in existing `.rdb` files.

use reddb_types::{
    ColumnDef, Constraint, ConstraintType, DataType, IndexDef, IndexType, TableDef, TableDefError,
};
use std::collections::HashMap;

/// Magic prefix for a serialized table definition.
pub const TABLE_DEF_MAGIC: [u8; 4] = *b"RTBL";

// ============================================================================
// Encode
// ============================================================================

/// Serialize a table-definition payload to bytes.
pub fn encode_table_def(table: &TableDef) -> Vec<u8> {
    let mut buf = Vec::new();

    buf.extend_from_slice(&TABLE_DEF_MAGIC);
    buf.extend_from_slice(&table.version.to_le_bytes());
    write_string(&mut buf, &table.name);
    buf.extend_from_slice(&table.created_at.to_le_bytes());
    buf.extend_from_slice(&table.updated_at.to_le_bytes());

    write_varint(&mut buf, table.columns.len() as u64);
    for col in &table.columns {
        write_column(&mut buf, col);
    }

    write_varint(&mut buf, table.primary_key.len() as u64);
    for pk in &table.primary_key {
        write_string(&mut buf, pk);
    }

    write_varint(&mut buf, table.indexes.len() as u64);
    for idx in &table.indexes {
        write_index(&mut buf, idx);
    }

    write_varint(&mut buf, table.constraints.len() as u64);
    for constraint in &table.constraints {
        write_constraint(&mut buf, constraint);
    }

    buf
}

fn write_column(buf: &mut Vec<u8>, col: &ColumnDef) {
    write_string(buf, &col.name);
    buf.push(col.data_type.to_byte());
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

    if let Some(element_type) = col.element_type {
        buf.push(1);
        buf.push(element_type.to_byte());
    } else {
        buf.push(0);
    }

    write_varint(buf, col.metadata.len() as u64);
    for (k, v) in &col.metadata {
        write_string(buf, k);
        write_string(buf, v);
    }
}

fn write_index(buf: &mut Vec<u8>, idx: &IndexDef) {
    write_string(buf, &idx.name);
    buf.push(idx.index_type as u8);
    buf.push(if idx.unique { 1 } else { 0 });
    write_varint(buf, idx.columns.len() as u64);
    for col in &idx.columns {
        write_string(buf, col);
    }
}

fn write_constraint(buf: &mut Vec<u8>, constraint: &Constraint) {
    write_string(buf, &constraint.name);
    buf.push(constraint.constraint_type as u8);

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
pub fn decode_table_def(data: &[u8]) -> Result<TableDef, TableDefError> {
    if data.len() < 4 {
        return Err(TableDefError::TruncatedData);
    }
    if data[0..4] != TABLE_DEF_MAGIC {
        return Err(TableDefError::InvalidMagic);
    }

    let mut offset = 4;

    if data.len() < offset + 4 {
        return Err(TableDefError::TruncatedData);
    }
    let version = u32::from_le_bytes(data[offset..offset + 4].try_into().expect("u32 checked"));
    offset += 4;

    let (name, name_len) = read_string(&data[offset..])?;
    offset += name_len;

    if data.len() < offset + 16 {
        return Err(TableDefError::TruncatedData);
    }
    let created_at = u64::from_le_bytes(data[offset..offset + 8].try_into().expect("u64 checked"));
    offset += 8;
    let updated_at = u64::from_le_bytes(data[offset..offset + 8].try_into().expect("u64 checked"));
    offset += 8;

    let (col_count, varint_len) = read_varint(&data[offset..])?;
    offset += varint_len;
    let mut columns = Vec::new();
    for _ in 0..col_count {
        let (col, col_len) = read_column(&data[offset..])?;
        offset += col_len;
        columns.push(col);
    }

    let (pk_count, varint_len) = read_varint(&data[offset..])?;
    offset += varint_len;
    let mut primary_key = Vec::new();
    for _ in 0..pk_count {
        let (pk, pk_len) = read_string(&data[offset..])?;
        offset += pk_len;
        primary_key.push(pk);
    }

    let (idx_count, varint_len) = read_varint(&data[offset..])?;
    offset += varint_len;
    let mut indexes = Vec::new();
    for _ in 0..idx_count {
        let (idx, idx_len) = read_index(&data[offset..])?;
        offset += idx_len;
        indexes.push(idx);
    }

    let (constraint_count, varint_len) = read_varint(&data[offset..])?;
    offset += varint_len;
    let mut constraints = Vec::new();
    for _ in 0..constraint_count {
        let (constraint, constraint_len) = read_constraint(&data[offset..])?;
        offset += constraint_len;
        constraints.push(constraint);
    }

    Ok(TableDef {
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

fn read_column(data: &[u8]) -> Result<(ColumnDef, usize), TableDefError> {
    let mut offset = 0;

    let (name, name_len) = read_string(&data[offset..])?;
    offset += name_len;

    if data.len() < offset + 2 {
        return Err(TableDefError::TruncatedData);
    }
    let data_type = DataType::from_byte(data[offset]).ok_or(TableDefError::InvalidDataType)?;
    offset += 1;
    let nullable = data[offset] != 0;
    offset += 1;

    if data.len() < offset + 1 {
        return Err(TableDefError::TruncatedData);
    }
    let has_default = data[offset] != 0;
    offset += 1;
    let default = if has_default {
        let (len, varint_len) = read_varint(&data[offset..])?;
        offset += varint_len;
        let len = usize::try_from(len).map_err(|_| TableDefError::TruncatedData)?;
        let end = offset
            .checked_add(len)
            .ok_or(TableDefError::TruncatedData)?;
        if data.len() < end {
            return Err(TableDefError::TruncatedData);
        }
        let default_data = data[offset..end].to_vec();
        offset = end;
        Some(default_data)
    } else {
        None
    };

    if data.len() < offset + 1 {
        return Err(TableDefError::TruncatedData);
    }
    let has_vector_dim = data[offset] != 0;
    offset += 1;
    let vector_dim = if has_vector_dim {
        if data.len() < offset + 4 {
            return Err(TableDefError::TruncatedData);
        }
        let dim = u32::from_le_bytes(data[offset..offset + 4].try_into().expect("u32 checked"));
        offset += 4;
        Some(dim)
    } else {
        None
    };

    if data.len() < offset + 1 {
        return Err(TableDefError::TruncatedData);
    }
    let compress = data[offset] != 0;
    offset += 1;

    let (variant_count, varint_len) = read_varint(&data[offset..])?;
    offset += varint_len;
    let mut enum_variants = Vec::new();
    for _ in 0..variant_count {
        let (variant, variant_len) = read_string(&data[offset..])?;
        offset += variant_len;
        enum_variants.push(variant);
    }

    if data.len() < offset + 1 {
        return Err(TableDefError::TruncatedData);
    }
    let decimal_precision = data[offset];
    offset += 1;

    if data.len() < offset + 1 {
        return Err(TableDefError::TruncatedData);
    }
    let has_element_type = data[offset] != 0;
    offset += 1;
    let element_type = if has_element_type {
        if data.len() < offset + 1 {
            return Err(TableDefError::TruncatedData);
        }
        let element_type =
            DataType::from_byte(data[offset]).ok_or(TableDefError::InvalidDataType)?;
        offset += 1;
        Some(element_type)
    } else {
        None
    };

    let (meta_count, varint_len) = read_varint(&data[offset..])?;
    offset += varint_len;
    let mut metadata = HashMap::new();
    for _ in 0..meta_count {
        let (k, k_len) = read_string(&data[offset..])?;
        offset += k_len;
        let (v, v_len) = read_string(&data[offset..])?;
        offset += v_len;
        metadata.insert(k, v);
    }

    Ok((
        ColumnDef {
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

fn read_index(data: &[u8]) -> Result<(IndexDef, usize), TableDefError> {
    let mut offset = 0;

    let (name, name_len) = read_string(&data[offset..])?;
    offset += name_len;

    if data.len() < offset + 2 {
        return Err(TableDefError::TruncatedData);
    }
    let index_type = IndexType::from_byte(data[offset]).ok_or(TableDefError::InvalidIndexType)?;
    offset += 1;
    let unique = data[offset] != 0;
    offset += 1;

    let (col_count, varint_len) = read_varint(&data[offset..])?;
    offset += varint_len;
    let mut columns = Vec::new();
    for _ in 0..col_count {
        let (col, col_len) = read_string(&data[offset..])?;
        offset += col_len;
        columns.push(col);
    }

    Ok((
        IndexDef {
            name,
            index_type,
            unique,
            columns,
        },
        offset,
    ))
}

fn read_constraint(data: &[u8]) -> Result<(Constraint, usize), TableDefError> {
    let mut offset = 0;

    let (name, name_len) = read_string(&data[offset..])?;
    offset += name_len;

    if data.len() < offset + 1 {
        return Err(TableDefError::TruncatedData);
    }
    let constraint_type =
        ConstraintType::from_byte(data[offset]).ok_or(TableDefError::InvalidConstraintType)?;
    offset += 1;

    let (col_count, varint_len) = read_varint(&data[offset..])?;
    offset += varint_len;
    let mut columns = Vec::new();
    for _ in 0..col_count {
        let (col, col_len) = read_string(&data[offset..])?;
        offset += col_len;
        columns.push(col);
    }

    if data.len() < offset + 1 {
        return Err(TableDefError::TruncatedData);
    }
    let has_ref = data[offset] != 0;
    offset += 1;

    let (ref_table, ref_columns) = if has_ref {
        let (table, table_len) = read_string(&data[offset..])?;
        offset += table_len;

        let (ref_col_count, varint_len) = read_varint(&data[offset..])?;
        offset += varint_len;
        let mut ref_cols = Vec::new();
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
        Constraint {
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

fn read_varint(data: &[u8]) -> Result<(u64, usize), TableDefError> {
    let mut result: u64 = 0;
    let mut shift = 0;
    let mut offset = 0;

    loop {
        if offset >= data.len() {
            return Err(TableDefError::TruncatedData);
        }
        let byte = data[offset];
        offset += 1;

        if shift >= 64 {
            return Err(TableDefError::VarintOverflow);
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

fn read_string(data: &[u8]) -> Result<(String, usize), TableDefError> {
    let (len, varint_len) = read_varint(data)?;
    let offset = varint_len;
    let len = usize::try_from(len).map_err(|_| TableDefError::TruncatedData)?;
    let end = offset
        .checked_add(len)
        .ok_or(TableDefError::TruncatedData)?;
    if data.len() < end {
        return Err(TableDefError::TruncatedData);
    }
    let s = String::from_utf8(data[offset..end].to_vec())
        .map_err(|_| TableDefError::TruncatedData)?;
    Ok((s, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_def_rejects_bad_input() {
        assert_eq!(
            decode_table_def(&[0u8; 2]),
            Err(TableDefError::TruncatedData)
        );
        let mut bad = encode_table_def(&TableDef::new("t"));
        bad[0] = b'X';
        assert_eq!(
            decode_table_def(&bad),
            Err(TableDefError::InvalidMagic)
        );
        let encoded = encode_table_def(&TableDef::new("t"));
        assert_eq!(
            decode_table_def(&encoded[..encoded.len() - 1]),
            Err(TableDefError::TruncatedData)
        );
    }

    #[test]
    fn table_def_does_not_preallocate_untrusted_counts() {
        let mut encoded = Vec::new();
        encoded.extend_from_slice(&TABLE_DEF_MAGIC);
        encoded.extend_from_slice(&1u32.to_le_bytes());
        write_string(&mut encoded, "t");
        encoded.extend_from_slice(&0u64.to_le_bytes());
        encoded.extend_from_slice(&0u64.to_le_bytes());
        write_varint(&mut encoded, u64::MAX);

        assert_eq!(
            decode_table_def(&encoded),
            Err(TableDefError::TruncatedData)
        );
    }
}
