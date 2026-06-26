//! Native binary container for document bodies (PRD-1398, ADR-0063).
//!
//! Encodes a named-field document (`&[(&str, &Value)]`) into a compact binary
//! container with a **header offset table** that enables O(1) access to any
//! top-level field value without decoding the entire document.
//!
//! ## Format — version 1
//!
//! ```text
//! [0..4]  magic        = b"RDOC"
//! [4]     version      = 0x01
//! [5..7]  num_fields   : u16 LE          (max 65 535 fields per document)
//! [7..]   offset table : num_fields × [key_len: u16 LE, val_offset: u32 LE]
//!         keys section : concatenated UTF-8 field name bytes (key_len[i] each)
//!         values section: concatenated value_codec-encoded bytes
//! ```
//!
//! `val_offset` is an **absolute** byte offset from byte 0 of the container,
//! allowing a single value to be decoded with [`decode_value_at_offset`] after
//! reading only the header — the rest of the payload stays untouched.
//!
//! ## Flag-dark
//!
//! This codec is compiled and tested but not yet wired into any storage or
//! query path.  Behaviour cutover happens in a later PRD-1398 slice.

use crate::key_dictionary::KeyDictionary;
use crate::types::Value;
use crate::value_codec;

/// Magic bytes at the start of every document body container.
pub const MAGIC: &[u8; 4] = b"RDOC";

/// Format version byte for the plain (inline-keys-only) container.
pub const VERSION: u8 = 0x01;

/// Format version byte for the dictionary-aware container (PRD-1398).
///
/// Identical layout to v1 except each offset-table entry carries a key *kind*
/// tag: a field name is either a key-id into the per-collection
/// [`KeyDictionary`] (common keys) or stored inline (rare/unique keys).
pub const VERSION_DICT: u8 = 0x02;

/// Byte size of one entry in the offset table: u16 key_len + u32 val_offset.
const ENTRY_SIZE: usize = 6;

/// Byte size of one v2 offset-table entry: u8 key_kind + u32 key_ref + u32 val_offset.
///
/// The key-id is stored as a fixed `u32` (rather than a varint) so the table
/// keeps a fixed stride and field access stays O(1); homogeneous collections
/// never approach `u32::MAX` distinct common keys.
const DICT_ENTRY_SIZE: usize = 9;

/// `key_kind` tag: the field name is a key-id into the [`KeyDictionary`].
const KEY_KIND_DICT: u8 = 0x00;

/// `key_kind` tag: the field name is stored inline in the keys section.
const KEY_KIND_INLINE: u8 = 0x01;

/// One parsed v2 offset-table entry: `(key_kind, key_ref, val_offset)`.
///
/// `key_ref` is a dictionary key-id when `key_kind == KEY_KIND_DICT`, or the
/// inline key length when `key_kind == KEY_KIND_INLINE`.
pub type DictEntry = (u8, u32, u32);

/// Errors produced by the document body codec.
#[derive(Debug, PartialEq)]
pub enum DocBodyError {
    /// Buffer is too short to hold the header or offset table.
    TruncatedData,
    /// First 4 bytes do not match `b"RDOC"`.
    BadMagic,
    /// Version byte is not `0x01`.
    UnsupportedVersion(u8),
    /// A field name or value points outside the container buffer.
    OffsetOutOfBounds,
    /// A field name is not valid UTF-8.
    InvalidFieldName,
    /// The document has more than 65 535 fields, or a field name exceeds 65 535 bytes.
    FieldLimitExceeded,
    /// A dictionary key-id in the body is absent from the supplied dictionary.
    UnknownKeyId(u32),
    /// A v2 offset-table entry carried an unrecognised key-kind tag.
    BadKeyKind(u8),
    /// The underlying value codec rejected a value.
    ValueCodecError(crate::types::ValueError),
}

impl From<crate::types::ValueError> for DocBodyError {
    fn from(e: crate::types::ValueError) -> Self {
        DocBodyError::ValueCodecError(e)
    }
}

impl std::fmt::Display for DocBodyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TruncatedData => write!(f, "document body: truncated data"),
            Self::BadMagic => write!(f, "document body: bad magic bytes (expected RDOC)"),
            Self::UnsupportedVersion(v) => write!(f, "document body: unsupported version {v}"),
            Self::OffsetOutOfBounds => write!(f, "document body: offset points outside buffer"),
            Self::InvalidFieldName => write!(f, "document body: field name is not valid UTF-8"),
            Self::FieldLimitExceeded => {
                write!(f, "document body: field or key-length limit exceeded")
            }
            Self::UnknownKeyId(id) => {
                write!(f, "document body: key-id {id} not found in dictionary")
            }
            Self::BadKeyKind(k) => write!(f, "document body: unrecognised key kind {k:#04x}"),
            Self::ValueCodecError(e) => write!(f, "document body: value codec error: {e}"),
        }
    }
}

impl std::error::Error for DocBodyError {}

/// Encode `fields` as a document body container, appending bytes to `out`.
///
/// Fields are written in iteration order.  Duplicate field names are allowed
/// (deduplication is the caller's responsibility).
///
/// Returns an error only if `fields.len() > 65535` or a field name is longer
/// than 65535 bytes; both are pathological in practice.
pub fn encode(fields: &[(&str, &Value)], out: &mut Vec<u8>) -> Result<(), DocBodyError> {
    let n = fields.len();
    if n > u16::MAX as usize {
        return Err(DocBodyError::FieldLimitExceeded);
    }

    // Encode all keys and values into scratch buffers, tracking per-field
    // sizes so we can compute absolute offsets before writing anything.
    let mut key_buf: Vec<u8> = Vec::new();
    let mut val_buf: Vec<u8> = Vec::new();
    let mut key_lens: Vec<u16> = Vec::with_capacity(n);
    let mut val_starts: Vec<u32> = Vec::with_capacity(n);

    for (key, value) in fields {
        let kb = key.as_bytes();
        if kb.len() > u16::MAX as usize {
            return Err(DocBodyError::FieldLimitExceeded);
        }
        key_lens.push(kb.len() as u16);
        key_buf.extend_from_slice(kb);

        val_starts.push(val_buf.len() as u32); // relative for now
        value_codec::encode(value, &mut val_buf);
    }

    // Absolute offset of the values section within the final container:
    //   magic(4) + version(1) + num_fields(2) + table(n * ENTRY_SIZE) + keys
    let vals_abs_start = 4 + 1 + 2 + n * ENTRY_SIZE + key_buf.len();

    // Write header
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&(n as u16).to_le_bytes());

    // Write offset table
    for i in 0..n {
        let abs_val_offset = vals_abs_start + val_starts[i] as usize;
        out.extend_from_slice(&key_lens[i].to_le_bytes());
        out.extend_from_slice(&(abs_val_offset as u32).to_le_bytes());
    }

    // Write keys then values
    out.extend_from_slice(&key_buf);
    out.extend_from_slice(&val_buf);

    Ok(())
}

/// Decode all fields from a document body container.
///
/// Fields are returned in the same order they were encoded.
pub fn decode(data: &[u8]) -> Result<Vec<(String, Value)>, DocBodyError> {
    let (n, table) = parse_header(data)?;
    let header_end = 7 + n * ENTRY_SIZE; // where the keys section begins
    let mut key_cursor = header_end;
    let mut result = Vec::with_capacity(n);

    for &(key_len, val_offset) in &table {
        // Read field name
        let key_end = key_cursor + key_len as usize;
        if key_end > data.len() {
            return Err(DocBodyError::OffsetOutOfBounds);
        }
        let key = std::str::from_utf8(&data[key_cursor..key_end])
            .map_err(|_| DocBodyError::InvalidFieldName)?
            .to_string();
        key_cursor = key_end;

        // Decode value by absolute offset
        let value = decode_value_at_offset(data, val_offset)?;
        result.push((key, value));
    }

    Ok(result)
}

/// Return the top-level field names in encode order without decoding any value.
///
/// Reads only the offset table and the keys section — the values section is
/// never touched. Used to build a field-name index/bloom over a stored body
/// (e.g. so a `WHERE`/projection on a promoted field can route to the body)
/// without paying the cost of a full [`decode`].
pub fn field_names(data: &[u8]) -> Result<Vec<String>, DocBodyError> {
    let (n, table) = parse_header(data)?;
    let header_end = 7 + n * ENTRY_SIZE; // where the keys section begins
    let mut key_cursor = header_end;
    let mut names = Vec::with_capacity(n);

    for &(key_len, _val_offset) in &table {
        let key_end = key_cursor + key_len as usize;
        if key_end > data.len() {
            return Err(DocBodyError::OffsetOutOfBounds);
        }
        let key = std::str::from_utf8(&data[key_cursor..key_end])
            .map_err(|_| DocBodyError::InvalidFieldName)?
            .to_string();
        key_cursor = key_end;
        names.push(key);
    }

    Ok(names)
}

/// Read a single field by name without decoding any other value.
///
/// Returns `None` when the field is absent.  Only the matching field's
/// encoded bytes are passed to [`value_codec::decode`]; everything else
/// is skipped as raw bytes.
pub fn read_field_by_name(data: &[u8], name: &str) -> Result<Option<Value>, DocBodyError> {
    let (n, table) = parse_header(data)?;
    let header_end = 7 + n * ENTRY_SIZE;
    let mut key_cursor = header_end;

    for &(key_len, val_offset) in &table {
        let key_end = key_cursor + key_len as usize;
        if key_end > data.len() {
            return Err(DocBodyError::OffsetOutOfBounds);
        }
        let key_bytes = &data[key_cursor..key_end];
        key_cursor = key_end;

        if key_bytes == name.as_bytes() {
            let value = decode_value_at_offset(data, val_offset)?;
            return Ok(Some(value));
        }
    }

    Ok(None)
}

/// Decode a single value using its **absolute** byte offset within the
/// container.
///
/// This is the O(1) access path once the caller has read the offset table
/// (e.g. via [`parse_header`]).  No other field is decoded.
pub fn decode_value_at_offset(data: &[u8], val_offset: u32) -> Result<Value, DocBodyError> {
    let off = val_offset as usize;
    if off >= data.len() {
        return Err(DocBodyError::OffsetOutOfBounds);
    }
    let (value, _) = value_codec::decode(&data[off..])?;
    Ok(value)
}

/// Parse the container header and return `(num_fields, offset_table)`.
///
/// Each table entry is `(key_len: u16, val_offset: u32)`.  The table may be
/// empty for zero-field documents.
///
/// Does **not** validate that field names or values are within bounds — that
/// is deferred to the decode functions that actually walk those sections.
pub fn parse_header(data: &[u8]) -> Result<(usize, Vec<(u16, u32)>), DocBodyError> {
    if data.len() < 7 {
        return Err(DocBodyError::TruncatedData);
    }
    if &data[0..4] != MAGIC.as_slice() {
        return Err(DocBodyError::BadMagic);
    }
    if data[4] != VERSION {
        return Err(DocBodyError::UnsupportedVersion(data[4]));
    }

    let n = u16::from_le_bytes([data[5], data[6]]) as usize;
    let table_end = 7 + n * ENTRY_SIZE;
    if data.len() < table_end {
        return Err(DocBodyError::TruncatedData);
    }

    let mut table = Vec::with_capacity(n);
    for i in 0..n {
        let base = 7 + i * ENTRY_SIZE;
        let key_len = u16::from_le_bytes([data[base], data[base + 1]]);
        let val_offset = u32::from_le_bytes([
            data[base + 2],
            data[base + 3],
            data[base + 4],
            data[base + 5],
        ]);
        table.push((key_len, val_offset));
    }

    Ok((n, table))
}

/// Encode `fields` as a **dictionary-aware** (v2) document body container.
///
/// For each field `classify(name)` decides how the key is stored:
///
/// * `true`  — the key is *common*: it is interned into `dict` (appending a new
///   id transactionally if it is not already present) and the body stores the
///   compact key-id.
/// * `false` — the key is *rare/unique*: it is stored **inline** in the body
///   and never enters `dict`, so a heterogeneous collection cannot bloat the
///   shared catalogue.
///
/// Decode with [`decode_with_dictionary`] using the (post-encode) dictionary.
pub fn encode_with_dictionary(
    fields: &[(&str, &Value)],
    dict: &mut KeyDictionary,
    classify: impl Fn(&str) -> bool,
    out: &mut Vec<u8>,
) -> Result<(), DocBodyError> {
    let n = fields.len();
    if n > u16::MAX as usize {
        return Err(DocBodyError::FieldLimitExceeded);
    }

    // Inline key bytes (only for rare keys) and value bytes, plus per-field
    // table data, are staged so absolute offsets can be computed up front.
    let mut key_buf: Vec<u8> = Vec::new();
    let mut val_buf: Vec<u8> = Vec::new();
    let mut kinds: Vec<u8> = Vec::with_capacity(n);
    let mut refs: Vec<u32> = Vec::with_capacity(n);
    let mut val_starts: Vec<u32> = Vec::with_capacity(n);

    for (key, value) in fields {
        if classify(key) {
            let id = dict.intern(key);
            kinds.push(KEY_KIND_DICT);
            refs.push(id);
        } else {
            let kb = key.as_bytes();
            if kb.len() > u16::MAX as usize {
                return Err(DocBodyError::FieldLimitExceeded);
            }
            kinds.push(KEY_KIND_INLINE);
            refs.push(kb.len() as u32);
            key_buf.extend_from_slice(kb);
        }
        val_starts.push(val_buf.len() as u32); // relative for now
        value_codec::encode(value, &mut val_buf);
    }

    // Absolute offset of the values section within the final container:
    //   magic(4) + version(1) + num_fields(2) + table(n * DICT_ENTRY_SIZE) + inline keys
    let vals_abs_start = 4 + 1 + 2 + n * DICT_ENTRY_SIZE + key_buf.len();

    out.extend_from_slice(MAGIC);
    out.push(VERSION_DICT);
    out.extend_from_slice(&(n as u16).to_le_bytes());

    for i in 0..n {
        let abs_val_offset = vals_abs_start + val_starts[i] as usize;
        out.push(kinds[i]);
        out.extend_from_slice(&refs[i].to_le_bytes());
        out.extend_from_slice(&(abs_val_offset as u32).to_le_bytes());
    }

    out.extend_from_slice(&key_buf);
    out.extend_from_slice(&val_buf);

    Ok(())
}

/// Decode all fields from a **dictionary-aware** (v2) document body container.
///
/// `dict` must be the per-collection dictionary that was used (and possibly
/// extended) during [`encode_with_dictionary`]; dictionary key-ids are resolved
/// back to field names through it, while inline keys are read straight from the
/// body.  Fields are returned in encode order.
pub fn decode_with_dictionary(
    data: &[u8],
    dict: &KeyDictionary,
) -> Result<Vec<(String, Value)>, DocBodyError> {
    let (n, table) = parse_dict_header(data)?;
    let header_end = 7 + n * DICT_ENTRY_SIZE; // where the inline-keys section begins
    let mut key_cursor = header_end;
    let mut result = Vec::with_capacity(n);

    for &(kind, key_ref, val_offset) in &table {
        let key = match kind {
            KEY_KIND_DICT => dict
                .name_of(key_ref)
                .ok_or(DocBodyError::UnknownKeyId(key_ref))?
                .to_string(),
            KEY_KIND_INLINE => {
                let key_end = key_cursor + key_ref as usize;
                if key_end > data.len() {
                    return Err(DocBodyError::OffsetOutOfBounds);
                }
                let name = std::str::from_utf8(&data[key_cursor..key_end])
                    .map_err(|_| DocBodyError::InvalidFieldName)?
                    .to_string();
                key_cursor = key_end;
                name
            }
            other => return Err(DocBodyError::BadKeyKind(other)),
        };

        let value = decode_value_at_offset(data, val_offset)?;
        result.push((key, value));
    }

    Ok(result)
}

/// Parse a **v2** container header, returning `(num_fields, table)` where each
/// table entry is `(key_kind, key_ref, val_offset)`.
///
/// As with [`parse_header`], inline key names and values are not validated
/// here — that is deferred to [`decode_with_dictionary`].
pub fn parse_dict_header(data: &[u8]) -> Result<(usize, Vec<DictEntry>), DocBodyError> {
    if data.len() < 7 {
        return Err(DocBodyError::TruncatedData);
    }
    if &data[0..4] != MAGIC.as_slice() {
        return Err(DocBodyError::BadMagic);
    }
    if data[4] != VERSION_DICT {
        return Err(DocBodyError::UnsupportedVersion(data[4]));
    }

    let n = u16::from_le_bytes([data[5], data[6]]) as usize;
    let table_end = 7 + n * DICT_ENTRY_SIZE;
    if data.len() < table_end {
        return Err(DocBodyError::TruncatedData);
    }

    let mut table = Vec::with_capacity(n);
    for i in 0..n {
        let base = 7 + i * DICT_ENTRY_SIZE;
        let kind = data[base];
        let key_ref = u32::from_le_bytes([
            data[base + 1],
            data[base + 2],
            data[base + 3],
            data[base + 4],
        ]);
        let val_offset = u32::from_le_bytes([
            data[base + 5],
            data[base + 6],
            data[base + 7],
            data[base + 8],
        ]);
        table.push((kind, key_ref, val_offset));
    }

    Ok((n, table))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Value;
    use std::net::{IpAddr, Ipv4Addr};

    fn round_trip(fields: &[(&str, Value)]) -> Vec<(String, Value)> {
        let refs: Vec<(&str, &Value)> = fields.iter().map(|(k, v)| (*k, v)).collect();
        let mut buf = Vec::new();
        encode(&refs, &mut buf).expect("encode");
        decode(&buf).expect("decode")
    }

    #[test]
    fn empty_document_round_trips() {
        let got = round_trip(&[]);
        assert!(got.is_empty());
    }

    #[test]
    fn single_field_round_trip() {
        let fields = [("name", Value::text("Alice"))];
        let got = round_trip(&fields);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "name");
        assert_eq!(got[0].1, Value::text("Alice"));
    }

    #[test]
    fn multi_field_round_trip() {
        let fields = [
            ("id", Value::Integer(42)),
            ("email", Value::Email("a@example.com".to_string())),
            ("active", Value::Boolean(true)),
            ("score", Value::Float(9.5)),
        ];
        let got = round_trip(&fields);
        assert_eq!(got.len(), 4);
        for (i, (k, v)) in fields.iter().enumerate() {
            assert_eq!(got[i].0, *k);
            assert_eq!(got[i].1, *v);
        }
    }

    /// Every rich semantic type must survive the round-trip unchanged.
    #[test]
    fn rich_semantic_types_round_trip() {
        let fields = [
            ("email", Value::Email("user@example.com".to_string())),
            ("ipv4", Value::Ipv4(0x7f000001)),
            ("subnet", Value::Subnet(0x0a000000, 0xff000000)),
            ("color", Value::Color([0xDE, 0xAD, 0xBE])),
            ("phone", Value::Phone(5511999000000)),
            ("semver", Value::Semver(1_002_003)),
            ("uuid", Value::Uuid([0xAB; 16])),
            (
                "money",
                Value::Money {
                    asset_code: "USD".to_string(),
                    minor_units: 9999,
                    scale: 2,
                },
            ),
            ("geo", Value::GeoPoint(-23_550_520, -46_633_308)),
            (
                "ip_mixed",
                Value::IpAddr(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))),
            ),
            ("url", Value::Url("https://reddb.io".to_string())),
            ("color_alpha", Value::ColorAlpha([1, 2, 3, 255])),
            ("lang", Value::Lang2(*b"en")),
            ("country", Value::Country3(*b"USA")),
        ];
        let got = round_trip(&fields);
        assert_eq!(got.len(), fields.len());
        for (i, (k, v)) in fields.iter().enumerate() {
            assert_eq!(got[i].0, *k, "key mismatch at {i}");
            assert_eq!(got[i].1, *v, "value mismatch for field {k}");
        }
    }

    /// A single field can be read by name without decoding the rest.
    #[test]
    fn read_field_by_name_skips_other_fields() {
        let fields = [
            ("a", Value::Integer(1)),
            ("b", Value::text("hello")),
            ("c", Value::Boolean(false)),
        ];
        let refs: Vec<(&str, &Value)> = fields.iter().map(|(k, v)| (*k, v)).collect();
        let mut buf = Vec::new();
        encode(&refs, &mut buf).expect("encode");

        let v = read_field_by_name(&buf, "b")
            .expect("lookup")
            .expect("found");
        assert_eq!(v, Value::text("hello"));

        let missing = read_field_by_name(&buf, "z").expect("lookup");
        assert!(missing.is_none());
    }

    /// `field_names` lists the keys in encode order without decoding values.
    #[test]
    fn field_names_lists_keys_in_encode_order() {
        let fields = [
            ("name", Value::text("Alice")),
            ("score", Value::Integer(30)),
            ("active", Value::Boolean(true)),
        ];
        let refs: Vec<(&str, &Value)> = fields.iter().map(|(k, v)| (*k, v)).collect();
        let mut buf = Vec::new();
        encode(&refs, &mut buf).expect("encode");

        let names = field_names(&buf).expect("field_names");
        assert_eq!(names, vec!["name", "score", "active"]);

        assert!(field_names(&[]).is_err());
    }

    /// decode_value_at_offset provides direct O(1) access given a known offset.
    #[test]
    fn decode_value_at_offset_matches_full_decode() {
        let fields = [("x", Value::Integer(100)), ("y", Value::Float(1.5))];
        let refs: Vec<(&str, &Value)> = fields.iter().map(|(k, v)| (*k, v)).collect();
        let mut buf = Vec::new();
        encode(&refs, &mut buf).expect("encode");

        let (n, table) = parse_header(&buf).expect("parse_header");
        assert_eq!(n, 2);

        for (i, (k, v)) in fields.iter().enumerate() {
            let (_key_len, val_offset) = table[i];
            let got = decode_value_at_offset(&buf, val_offset).expect("decode_at_offset");
            assert_eq!(got, *v, "field {k} mismatch");
        }
    }

    #[test]
    fn rejects_bad_magic() {
        let mut buf = vec![b'X', b'X', b'X', b'X', VERSION, 0, 0];
        assert_eq!(decode(&buf), Err(DocBodyError::BadMagic));
        buf[0..4].copy_from_slice(MAGIC);
        buf[4] = 0x99;
        assert_eq!(decode(&buf), Err(DocBodyError::UnsupportedVersion(0x99)));
    }

    #[test]
    fn rejects_truncated_buffer() {
        assert_eq!(decode(&[]), Err(DocBodyError::TruncatedData));
        assert_eq!(
            decode(&[b'R', b'D', b'O', b'C', 1, 0]),
            Err(DocBodyError::TruncatedData)
        );
    }

    #[test]
    fn null_values_round_trip() {
        let fields = [("nothing", Value::Null), ("something", Value::Integer(7))];
        let got = round_trip(&fields);
        assert_eq!(got[0].1, Value::Null);
        assert_eq!(got[1].1, Value::Integer(7));
    }

    #[test]
    fn array_value_round_trip() {
        let fields = [(
            "tags",
            Value::Array(vec![Value::text("a"), Value::text("b"), Value::Integer(3)]),
        )];
        let got = round_trip(&fields);
        assert_eq!(
            got[0].1,
            Value::Array(vec![Value::text("a"), Value::text("b"), Value::Integer(3)])
        );
    }

    /// Verify the offset table points to the correct byte positions.
    #[test]
    fn offset_table_offsets_are_valid() {
        let fields = [("k", Value::Boolean(true))];
        let refs: Vec<(&str, &Value)> = fields.iter().map(|(k, v)| (*k, v)).collect();
        let mut buf = Vec::new();
        encode(&refs, &mut buf).expect("encode");

        let (n, table) = parse_header(&buf).expect("header");
        assert_eq!(n, 1);
        let (_klen, val_offset) = table[0];
        // The value bytes at val_offset must decode to Boolean(true)
        let (v, _) = value_codec::decode(&buf[val_offset as usize..]).expect("decode at offset");
        assert_eq!(v, Value::Boolean(true));
    }

    // ---- Dictionary-aware (v2) container ----------------------------------

    /// Classify every key as common — exercises the homogeneous-collection path.
    fn all_common(_: &str) -> bool {
        true
    }

    /// Common field names are interned in the dictionary and the body stores
    /// compact key-ids, not the key strings.
    #[test]
    fn common_keys_are_interned_as_key_ids() {
        let fields = [
            ("id", Value::Integer(1)),
            ("name", Value::text("Alice")),
            ("active", Value::Boolean(true)),
        ];
        let refs: Vec<(&str, &Value)> = fields.iter().map(|(k, v)| (*k, v)).collect();
        let mut dict = KeyDictionary::new();
        let mut buf = Vec::new();
        encode_with_dictionary(&refs, &mut dict, all_common, &mut buf).expect("encode");

        // All three keys entered the dictionary with dense ids.
        assert_eq!(dict.len(), 3);
        assert_eq!(dict.id_of("id"), Some(0));
        assert_eq!(dict.id_of("name"), Some(1));
        assert_eq!(dict.id_of("active"), Some(2));

        // Every table entry is a dictionary reference, no inline keys.
        let (n, table) = parse_dict_header(&buf).expect("header");
        assert_eq!(n, 3);
        for (kind, _key_ref, _off) in &table {
            assert_eq!(*kind, KEY_KIND_DICT);
        }
        // The string "name" is not repeated in the body bytes — only its id is.
        assert!(!buf.windows(4).any(|w| w == b"name"));
    }

    /// Encoding a second document with a new common key appends to the existing
    /// dictionary transactionally (alongside the write), reusing prior ids.
    #[test]
    fn new_common_key_appends_to_dictionary() {
        let mut dict = KeyDictionary::new();

        let doc1 = [("id", Value::Integer(1)), ("name", Value::text("Alice"))];
        let r1: Vec<(&str, &Value)> = doc1.iter().map(|(k, v)| (*k, v)).collect();
        let mut b1 = Vec::new();
        encode_with_dictionary(&r1, &mut dict, all_common, &mut b1).expect("encode 1");
        assert_eq!(dict.len(), 2);

        // Second document reuses id/name and introduces a brand-new common key.
        let doc2 = [
            ("id", Value::Integer(2)),
            ("name", Value::text("Bob")),
            ("email", Value::Email("bob@example.com".to_string())),
        ];
        let r2: Vec<(&str, &Value)> = doc2.iter().map(|(k, v)| (*k, v)).collect();
        let mut b2 = Vec::new();
        encode_with_dictionary(&r2, &mut dict, all_common, &mut b2).expect("encode 2");

        // Only the new key appended; existing ids are stable.
        assert_eq!(dict.len(), 3);
        assert_eq!(dict.id_of("id"), Some(0));
        assert_eq!(dict.id_of("name"), Some(1));
        assert_eq!(dict.id_of("email"), Some(2));

        // Both documents decode losslessly against the shared dictionary.
        let g1 = decode_with_dictionary(&b1, &dict).expect("decode 1");
        assert_eq!(g1[1], ("name".to_string(), Value::text("Alice")));
        let g2 = decode_with_dictionary(&b2, &dict).expect("decode 2");
        assert_eq!(
            g2[2],
            (
                "email".to_string(),
                Value::Email("bob@example.com".to_string())
            )
        );
    }

    /// A rare/unique key is stored inline and never enters the dictionary.
    #[test]
    fn rare_key_stays_inline_and_out_of_dictionary() {
        // "id"/"name" are common; anything else is rare.
        let common = |k: &str| k == "id" || k == "name";
        let fields = [
            ("id", Value::Integer(7)),
            ("name", Value::text("Carol")),
            ("x9f3_one_off_attr", Value::text("rare-value")),
        ];
        let refs: Vec<(&str, &Value)> = fields.iter().map(|(k, v)| (*k, v)).collect();
        let mut dict = KeyDictionary::new();
        let mut buf = Vec::new();
        encode_with_dictionary(&refs, &mut dict, common, &mut buf).expect("encode");

        // The rare key never entered the catalogue.
        assert_eq!(dict.len(), 2);
        assert_eq!(dict.id_of("x9f3_one_off_attr"), None);

        // Its kind tag is inline and its name bytes live in the body.
        let (_n, table) = parse_dict_header(&buf).expect("header");
        assert_eq!(table[0].0, KEY_KIND_DICT);
        assert_eq!(table[1].0, KEY_KIND_DICT);
        assert_eq!(table[2].0, KEY_KIND_INLINE);
        assert!(buf
            .windows("x9f3_one_off_attr".len())
            .any(|w| w == b"x9f3_one_off_attr"));

        // And it round-trips losslessly.
        let got = decode_with_dictionary(&buf, &dict).expect("decode");
        assert_eq!(
            got[2],
            ("x9f3_one_off_attr".to_string(), Value::text("rare-value"))
        );
    }

    /// Round-trip mixing dictionary keys and inline keys is lossless, including
    /// rich semantic types and after persisting/reloading the dictionary.
    #[test]
    fn mixed_dictionary_and_inline_round_trip() {
        let common = |k: &str| matches!(k, "id" | "email" | "active");
        let fields = [
            ("id", Value::Integer(42)),
            ("email", Value::Email("a@example.com".to_string())),
            ("active", Value::Boolean(true)),
            ("one_off_color", Value::Color([0xDE, 0xAD, 0xBE])),
            ("rare_geo", Value::GeoPoint(-23_550_520, -46_633_308)),
        ];
        let refs: Vec<(&str, &Value)> = fields.iter().map(|(k, v)| (*k, v)).collect();
        let mut dict = KeyDictionary::new();
        let mut buf = Vec::new();
        encode_with_dictionary(&refs, &mut dict, common, &mut buf).expect("encode");

        // Persist and reload the dictionary, then decode against the reloaded copy.
        let mut dict_bytes = Vec::new();
        dict.encode(&mut dict_bytes);
        let reloaded = KeyDictionary::decode(&dict_bytes).expect("reload dict");

        let got = decode_with_dictionary(&buf, &reloaded).expect("decode");
        assert_eq!(got.len(), fields.len());
        for (i, (k, v)) in fields.iter().enumerate() {
            assert_eq!(got[i].0, *k, "key mismatch at {i}");
            assert_eq!(got[i].1, *v, "value mismatch for field {k}");
        }
    }

    #[test]
    fn empty_dictionary_document_round_trips() {
        let mut dict = KeyDictionary::new();
        let mut buf = Vec::new();
        encode_with_dictionary(&[], &mut dict, all_common, &mut buf).expect("encode");
        let got = decode_with_dictionary(&buf, &dict).expect("decode");
        assert!(got.is_empty());
        assert!(dict.is_empty());
    }

    /// A v2 body decoded against a dictionary that lacks the id is rejected,
    /// not silently mis-decoded.
    #[test]
    fn unknown_key_id_is_rejected() {
        let fields = [("only_key", Value::Integer(1))];
        let refs: Vec<(&str, &Value)> = fields.iter().map(|(k, v)| (*k, v)).collect();
        let mut dict = KeyDictionary::new();
        let mut buf = Vec::new();
        encode_with_dictionary(&refs, &mut dict, all_common, &mut buf).expect("encode");

        let empty = KeyDictionary::new();
        assert_eq!(
            decode_with_dictionary(&buf, &empty),
            Err(DocBodyError::UnknownKeyId(0))
        );
    }

    /// v1 and v2 containers are distinguished by their version byte; decoding a
    /// v2 body with the v1 path (and vice versa) fails on the version check.
    #[test]
    fn v1_and_v2_versions_do_not_alias() {
        let fields = [("k", Value::Integer(1))];
        let refs: Vec<(&str, &Value)> = fields.iter().map(|(k, v)| (*k, v)).collect();

        let mut v1 = Vec::new();
        encode(&refs, &mut v1).expect("v1");
        assert_eq!(v1[4], VERSION);

        let mut dict = KeyDictionary::new();
        let mut v2 = Vec::new();
        encode_with_dictionary(&refs, &mut dict, all_common, &mut v2).expect("v2");
        assert_eq!(v2[4], VERSION_DICT);

        // Cross-decoding hits the version guard.
        assert_eq!(
            decode(&v2),
            Err(DocBodyError::UnsupportedVersion(VERSION_DICT))
        );
        assert_eq!(
            parse_dict_header(&v1),
            Err(DocBodyError::UnsupportedVersion(VERSION))
        );
    }
}
