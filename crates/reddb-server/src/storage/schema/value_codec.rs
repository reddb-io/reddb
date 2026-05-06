//! On-disk codec registry for [`Value`].
//!
//! This module is the **single source of truth** for the byte layout
//! of every [`Value`] variant. Adding a new variant means:
//!
//! 1. Add the variant to [`Value`].
//! 2. Add the matching [`DataType`] tag (the on-disk type byte).
//! 3. Add an arm to [`encode`] and [`decode`] in this file.
//!
//! That's it — no other file needs to learn the layout. The inherent
//! [`Value::to_bytes`] / [`Value::from_bytes`] methods stay as the
//! public API, but they only delegate here.
//!
//! ## Why a registry
//!
//! Before this module the encode / decode arms lived inside
//! `types.rs`, mixed with display / coercion / hashing logic. A
//! parallel `value_type_tag` helper in `storage::query` carried a
//! third numbering scheme. The result was that every new variant
//! required edits in three or more places and the tag spaces were
//! free to drift.
//!
//! With the registry there is exactly one mapping
//! `Value <-> on-disk bytes`. The wire protocol keeps its own,
//! independent `VAL_*` tag space (see `wire/protocol.rs`); the two
//! were never identical and any future unification is out of scope.
//!
//! ## On-disk format
//!
//! Bytes are unchanged versus the previous in-place implementation.
//! The pinned-byte regression test [`tests::pinned_bytes`] guards
//! the layout for the canonical variants (Null, Integer, Text, Bool,
//! Blob).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use super::types::{read_varint, write_varint, DataType, Value, ValueError};

/// Alias kept for callers that prefer the registry's own name. The
/// on-disk tag space is owned by [`DataType`]; `ValueKind` reads
/// better in registry contexts where the type name is the schema
/// label rather than a parser concept.
pub type ValueKind = DataType;

/// On-disk tag byte for a value.
///
/// `Value::Null` uses tag `0` (the same byte the legacy code reserved
/// as the explicit null marker before the [`DataType`] enum existed).
/// Every other variant returns `data_type().to_byte()`.
#[inline]
pub fn type_tag(value: &Value) -> u8 {
    match value {
        Value::Null => 0,
        other => other.data_type().to_byte(),
    }
}

/// Reverse lookup for [`type_tag`]. Returns `None` for unknown bytes;
/// `Some(DataType::Nullable)` for the dedicated null marker `0`.
#[inline]
pub fn type_for_tag(tag: u8) -> Option<ValueKind> {
    if tag == 0 {
        Some(DataType::Nullable)
    } else {
        DataType::from_byte(tag)
    }
}

/// C3 TOAST: minimum byte length to attempt zstd compression. Values
/// shorter than this are stored uncompressed — compression overhead
/// (~50 ns + header bytes) outweighs savings for small values.
pub(super) const TOAST_THRESHOLD: usize = 2048;

/// zstd compression level for TOAST values. Level 3 is PG's default
/// (balanced speed/ratio).
pub(super) const TOAST_ZSTD_LEVEL: i32 = 3;

/// Encode a value into `out`, appending its on-disk byte sequence.
///
/// The first byte is always [`type_tag`] of `value`; the remainder
/// is the variant-specific payload.
pub fn encode(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Null => {
            out.push(0); // Null marker
        }
        Value::Integer(v) => {
            out.push(DataType::Integer.to_byte());
            out.extend_from_slice(&v.to_le_bytes());
        }
        Value::UnsignedInteger(v) => {
            out.push(DataType::UnsignedInteger.to_byte());
            out.extend_from_slice(&v.to_le_bytes());
        }
        Value::Float(v) => {
            out.push(DataType::Float.to_byte());
            out.extend_from_slice(&v.to_le_bytes());
        }
        Value::Text(s) => {
            let bytes = s.as_bytes();
            // C3 TOAST: compress text values larger than the threshold.
            // Stores with `TextZstd` type byte when compression wins;
            // falls back to plain `Text` for small values or when zstd
            // doesn't reduce the size (e.g. already-compressed content).
            if bytes.len() > TOAST_THRESHOLD {
                if let Ok(compressed) = zstd::bulk::compress(bytes, TOAST_ZSTD_LEVEL) {
                    if compressed.len() < bytes.len() {
                        out.push(DataType::TextZstd.to_byte());
                        // original length first (needed to pre-allocate decompression buffer)
                        write_varint(out, bytes.len() as u64);
                        write_varint(out, compressed.len() as u64);
                        out.extend_from_slice(&compressed);
                        return;
                    }
                }
            }
            out.push(DataType::Text.to_byte());
            write_varint(out, bytes.len() as u64);
            out.extend_from_slice(bytes);
        }
        Value::Blob(data) => {
            // C3 TOAST: same pattern as Text.
            if data.len() > TOAST_THRESHOLD {
                if let Ok(compressed) = zstd::bulk::compress(data, TOAST_ZSTD_LEVEL) {
                    if compressed.len() < data.len() {
                        out.push(DataType::BlobZstd.to_byte());
                        write_varint(out, data.len() as u64);
                        write_varint(out, compressed.len() as u64);
                        out.extend_from_slice(&compressed);
                        return;
                    }
                }
            }
            out.push(DataType::Blob.to_byte());
            write_varint(out, data.len() as u64);
            out.extend_from_slice(data);
        }
        Value::Boolean(v) => {
            out.push(DataType::Boolean.to_byte());
            out.push(if *v { 1 } else { 0 });
        }
        Value::Timestamp(v) => {
            out.push(DataType::Timestamp.to_byte());
            out.extend_from_slice(&v.to_le_bytes());
        }
        Value::Duration(v) => {
            out.push(DataType::Duration.to_byte());
            out.extend_from_slice(&v.to_le_bytes());
        }
        Value::IpAddr(addr) => {
            out.push(DataType::IpAddr.to_byte());
            match addr {
                IpAddr::V4(v4) => {
                    out.push(4); // IPv4 marker
                    out.extend_from_slice(&v4.octets());
                }
                IpAddr::V6(v6) => {
                    out.push(6); // IPv6 marker
                    out.extend_from_slice(&v6.octets());
                }
            }
        }
        Value::MacAddr(mac) => {
            out.push(DataType::MacAddr.to_byte());
            out.extend_from_slice(mac);
        }
        Value::Vector(vec) => {
            out.push(DataType::Vector.to_byte());
            write_varint(out, vec.len() as u64);
            for v in vec {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        Value::Json(data) => {
            out.push(DataType::Json.to_byte());
            write_varint(out, data.len() as u64);
            out.extend_from_slice(data);
        }
        Value::Uuid(uuid) => {
            out.push(DataType::Uuid.to_byte());
            out.extend_from_slice(uuid);
        }
        Value::NodeRef(node_id) => {
            out.push(DataType::NodeRef.to_byte());
            let bytes = node_id.as_bytes();
            write_varint(out, bytes.len() as u64);
            out.extend_from_slice(bytes);
        }
        Value::EdgeRef(edge_id) => {
            out.push(DataType::EdgeRef.to_byte());
            let bytes = edge_id.as_bytes();
            write_varint(out, bytes.len() as u64);
            out.extend_from_slice(bytes);
        }
        Value::VectorRef(collection, vector_id) => {
            out.push(DataType::VectorRef.to_byte());
            let coll_bytes = collection.as_bytes();
            write_varint(out, coll_bytes.len() as u64);
            out.extend_from_slice(coll_bytes);
            out.extend_from_slice(&vector_id.to_le_bytes());
        }
        Value::RowRef(table, row_id) => {
            out.push(DataType::RowRef.to_byte());
            let table_bytes = table.as_bytes();
            write_varint(out, table_bytes.len() as u64);
            out.extend_from_slice(table_bytes);
            out.extend_from_slice(&row_id.to_le_bytes());
        }
        Value::Color(rgb) => {
            out.push(DataType::Color.to_byte());
            out.extend_from_slice(rgb);
        }
        Value::Email(s) => {
            out.push(DataType::Email.to_byte());
            let bytes = s.as_bytes();
            write_varint(out, bytes.len() as u64);
            out.extend_from_slice(bytes);
        }
        Value::Url(s) => {
            out.push(DataType::Url.to_byte());
            let bytes = s.as_bytes();
            write_varint(out, bytes.len() as u64);
            out.extend_from_slice(bytes);
        }
        Value::Phone(n) => {
            out.push(DataType::Phone.to_byte());
            out.extend_from_slice(&n.to_le_bytes());
        }
        Value::Semver(packed) => {
            out.push(DataType::Semver.to_byte());
            out.extend_from_slice(&packed.to_le_bytes());
        }
        Value::Cidr(ip, prefix) => {
            out.push(DataType::Cidr.to_byte());
            out.extend_from_slice(&ip.to_le_bytes());
            out.push(*prefix);
        }
        Value::Date(days) => {
            out.push(DataType::Date.to_byte());
            out.extend_from_slice(&days.to_le_bytes());
        }
        Value::Time(ms) => {
            out.push(DataType::Time.to_byte());
            out.extend_from_slice(&ms.to_le_bytes());
        }
        Value::Decimal(v) => {
            out.push(DataType::Decimal.to_byte());
            out.extend_from_slice(&v.to_le_bytes());
        }
        Value::EnumValue(idx) => {
            out.push(DataType::Enum.to_byte());
            out.push(*idx);
        }
        Value::Array(elements) => {
            out.push(DataType::Array.to_byte());
            write_varint(out, elements.len() as u64);
            for elem in elements {
                encode(elem, out);
            }
        }
        Value::TimestampMs(v) => {
            out.push(DataType::TimestampMs.to_byte());
            out.extend_from_slice(&v.to_le_bytes());
        }
        Value::Ipv4(v) => {
            out.push(DataType::Ipv4.to_byte());
            out.extend_from_slice(&v.to_le_bytes());
        }
        Value::Ipv6(bytes) => {
            out.push(DataType::Ipv6.to_byte());
            out.extend_from_slice(bytes);
        }
        Value::Subnet(ip, mask) => {
            out.push(DataType::Subnet.to_byte());
            out.extend_from_slice(&ip.to_le_bytes());
            out.extend_from_slice(&mask.to_le_bytes());
        }
        Value::Port(v) => {
            out.push(DataType::Port.to_byte());
            out.extend_from_slice(&v.to_le_bytes());
        }
        Value::Latitude(v) => {
            out.push(DataType::Latitude.to_byte());
            out.extend_from_slice(&v.to_le_bytes());
        }
        Value::Longitude(v) => {
            out.push(DataType::Longitude.to_byte());
            out.extend_from_slice(&v.to_le_bytes());
        }
        Value::GeoPoint(lat, lon) => {
            out.push(DataType::GeoPoint.to_byte());
            out.extend_from_slice(&lat.to_le_bytes());
            out.extend_from_slice(&lon.to_le_bytes());
        }
        Value::Country2(c) => {
            out.push(DataType::Country2.to_byte());
            out.extend_from_slice(c);
        }
        Value::Country3(c) => {
            out.push(DataType::Country3.to_byte());
            out.extend_from_slice(c);
        }
        Value::Lang2(c) => {
            out.push(DataType::Lang2.to_byte());
            out.extend_from_slice(c);
        }
        Value::Lang5(c) => {
            out.push(DataType::Lang5.to_byte());
            out.extend_from_slice(c);
        }
        Value::Currency(c) => {
            out.push(DataType::Currency.to_byte());
            out.extend_from_slice(c);
        }
        Value::AssetCode(code) => {
            out.push(DataType::AssetCode.to_byte());
            let bytes = code.as_bytes();
            write_varint(out, bytes.len() as u64);
            out.extend_from_slice(bytes);
        }
        Value::Money {
            asset_code,
            minor_units,
            scale,
        } => {
            out.push(DataType::Money.to_byte());
            let bytes = asset_code.as_bytes();
            write_varint(out, bytes.len() as u64);
            out.extend_from_slice(bytes);
            out.push(*scale);
            out.extend_from_slice(&minor_units.to_le_bytes());
        }
        Value::ColorAlpha(rgba) => {
            out.push(DataType::ColorAlpha.to_byte());
            out.extend_from_slice(rgba);
        }
        Value::BigInt(v) => {
            out.push(DataType::BigInt.to_byte());
            out.extend_from_slice(&v.to_le_bytes());
        }
        Value::KeyRef(col, key) => {
            out.push(DataType::KeyRef.to_byte());
            let col_bytes = col.as_bytes();
            write_varint(out, col_bytes.len() as u64);
            out.extend_from_slice(col_bytes);
            let key_bytes = key.as_bytes();
            write_varint(out, key_bytes.len() as u64);
            out.extend_from_slice(key_bytes);
        }
        Value::DocRef(col, id) => {
            out.push(DataType::DocRef.to_byte());
            let col_bytes = col.as_bytes();
            write_varint(out, col_bytes.len() as u64);
            out.extend_from_slice(col_bytes);
            out.extend_from_slice(&id.to_le_bytes());
        }
        Value::TableRef(name) => {
            out.push(DataType::TableRef.to_byte());
            let name_bytes = name.as_bytes();
            write_varint(out, name_bytes.len() as u64);
            out.extend_from_slice(name_bytes);
        }
        Value::PageRef(page_id) => {
            out.push(DataType::PageRef.to_byte());
            out.extend_from_slice(&page_id.to_le_bytes());
        }
        Value::Secret(bytes) => {
            out.push(DataType::Secret.to_byte());
            write_varint(out, bytes.len() as u64);
            out.extend_from_slice(bytes);
        }
        Value::Password(hash) => {
            out.push(DataType::Password.to_byte());
            let bytes = hash.as_bytes();
            write_varint(out, bytes.len() as u64);
            out.extend_from_slice(bytes);
        }
    }
}

/// Decode a single value from `data`, returning the value and the
/// number of bytes consumed.
pub fn decode(data: &[u8]) -> Result<(Value, usize), ValueError> {
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
            Value::text(s)
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
                let (elem, elem_size) = decode(&data[offset..])?;
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
        // Wire: encode writes TextZstd when text > TOAST_THRESHOLD and
        // compression saves space; decode always materialises as Value::Text.
        DataType::TextZstd => {
            let (orig_len, vs1) = read_varint(&data[offset..])?;
            offset += vs1;
            let (comp_len, vs2) = read_varint(&data[offset..])?;
            offset += vs2;
            if data.len() < offset + comp_len as usize {
                return Err(ValueError::TruncatedData);
            }
            let compressed = &data[offset..offset + comp_len as usize];
            let mut decompressed = vec![0u8; orig_len as usize];
            zstd::bulk::decompress_to_buffer(compressed, &mut decompressed)
                .map_err(|_| ValueError::InvalidUtf8)?;
            offset += comp_len as usize;
            let s = String::from_utf8(decompressed).map_err(|_| ValueError::InvalidUtf8)?;
            Value::text(s)
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
            let mut decompressed = vec![0u8; orig_len as usize];
            zstd::bulk::decompress_to_buffer(compressed, &mut decompressed)
                .map_err(|_| ValueError::InvalidUtf8)?;
            offset += comp_len as usize;
            Value::Blob(decompressed)
        }
    };

    Ok((value, offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinned on-disk byte layout for the canonical [`Value`]
    /// variants. **If this test breaks, callers with persisted data
    /// will fail to read older files** — only update the expected
    /// bytes when you have intentionally migrated the format. A
    /// silent rewrite is a corruption bug.
    ///
    /// Variants pinned: Null, Integer, Text, Boolean, Blob — the
    /// minimum five required by the codec registry contract.
    #[test]
    fn pinned_bytes() {
        // Null: just the null marker (0x00).
        let mut buf = Vec::new();
        encode(&Value::Null, &mut buf);
        assert_eq!(buf, vec![0x00], "Value::Null layout drifted");

        // Integer(-1): tag (Integer = 1) + i64 little-endian.
        let mut buf = Vec::new();
        encode(&Value::Integer(-1), &mut buf);
        assert_eq!(
            buf,
            vec![0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
            "Value::Integer layout drifted"
        );

        // Text("hi"): tag (Text = 4) + varint(2) + UTF-8 bytes.
        let mut buf = Vec::new();
        encode(&Value::text("hi"), &mut buf);
        assert_eq!(
            buf,
            vec![0x04, 0x02, b'h', b'i'],
            "Value::Text layout drifted"
        );

        // Boolean(true): tag (Boolean = 6) + 0x01.
        let mut buf = Vec::new();
        encode(&Value::Boolean(true), &mut buf);
        assert_eq!(buf, vec![0x06, 0x01], "Value::Boolean layout drifted");

        // Blob([0x01, 0x02, 0x03]): tag (Blob = 5) + varint(3) + raw.
        let mut buf = Vec::new();
        encode(&Value::Blob(vec![0x01, 0x02, 0x03]), &mut buf);
        assert_eq!(
            buf,
            vec![0x05, 0x03, 0x01, 0x02, 0x03],
            "Value::Blob layout drifted"
        );
    }

    /// Sanity check that the registry's [`type_tag`] lines up with
    /// [`DataType::to_byte`] for every storable variant — this is
    /// what guarantees the on-disk tag space stays single-source.
    #[test]
    fn type_tag_matches_data_type_byte() {
        let samples: &[Value] = &[
            Value::Null,
            Value::Integer(0),
            Value::UnsignedInteger(0),
            Value::Float(0.0),
            Value::text(""),
            Value::Blob(Vec::new()),
            Value::Boolean(false),
            Value::Timestamp(0),
            Value::Duration(0),
            Value::Uuid([0; 16]),
        ];
        for v in samples {
            let tag = type_tag(v);
            if matches!(v, Value::Null) {
                assert_eq!(tag, 0);
            } else {
                assert_eq!(tag, v.data_type().to_byte());
                let kind = type_for_tag(tag).expect("registered tag");
                assert_eq!(kind, v.data_type());
            }
        }
    }

    /// Decoder must reject a type byte it does not recognise rather
    /// than silently returning a default. Guards against on-disk
    /// corruption being interpreted as a valid value.
    #[test]
    fn rejects_unknown_type_tag() {
        // 0xFF is outside the registered DataType range.
        let buf = [0xFFu8];
        let err = decode(&buf).expect_err("unknown tag must error");
        assert!(matches!(err, ValueError::InvalidType(0xFF)));
    }

    /// A buffer truncated mid-payload must surface as
    /// `TruncatedData`, not panic on a slice index. Covers the
    /// fixed-width and length-prefixed code paths.
    #[test]
    fn rejects_truncated_buffer() {
        // Empty buffer.
        assert!(matches!(decode(&[]), Err(ValueError::EmptyData)));

        // Integer tag (0x01) needs 8 payload bytes; supply 3.
        let mut buf = vec![DataType::Integer.to_byte()];
        buf.extend_from_slice(&[0x01, 0x02, 0x03]);
        assert!(matches!(decode(&buf), Err(ValueError::TruncatedData)));

        // Text tag (0x04) with varint len=5 but only 2 payload bytes.
        let mut buf = vec![DataType::Text.to_byte()];
        write_varint(&mut buf, 5);
        buf.extend_from_slice(b"ab");
        assert!(matches!(decode(&buf), Err(ValueError::TruncatedData)));
    }

    /// Round-trip: encode then decode must recover the original
    /// value, byte for byte.
    #[test]
    fn round_trip_canonical_variants() {
        let cases = vec![
            Value::Null,
            Value::Integer(-12345),
            Value::text("hello"),
            Value::Boolean(true),
            Value::Blob(vec![1, 2, 3, 4, 5]),
        ];
        for original in cases {
            let mut bytes = Vec::new();
            encode(&original, &mut bytes);
            let (recovered, consumed) = decode(&bytes).expect("decode");
            assert_eq!(consumed, bytes.len());
            assert_eq!(original, recovered);
        }
    }
}
