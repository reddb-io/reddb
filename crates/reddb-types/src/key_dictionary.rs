//! Per-collection field-name key dictionary (PRD-1398, ADR-0063).
//!
//! Homogeneous document collections repeat the same handful of field-name
//! strings in every document.  This dictionary **interns** those common field
//! names into a per-collection, **append-only** name↔id catalogue so the
//! binary body can store a compact key-id (see [`document_body_codec`]) instead
//! of repeating the string.
//!
//! ## Append-only
//!
//! Ids are assigned in insertion order and never change or get reused once
//! assigned, so a key-id baked into a stored document body is stable for the
//! life of the collection.  This is what lets older document versions (ADR
//! 0014 MVCC) share the same dictionary as newer ones.
//!
//! ## Inline-key fallback
//!
//! The dictionary deliberately interns **only common keys**.  Rare or unique
//! field names (e.g. a heterogeneous collection where almost every document
//! carries a distinct key) are stored *inline* in the body and never enter the
//! catalogue, so an adversarial write pattern cannot bloat the shared
//! dictionary.  The common/rare classification policy lives at the write path;
//! this module only provides the catalogue primitive.
//!
//! ## Flag-dark
//!
//! Like the body codec, this is compiled and tested but not yet wired into any
//! storage path.  Persistence in the collection catalogue happens in a later
//! PRD-1398 slice.
//!
//! [`document_body_codec`]: crate::document_body_codec

use crate::types::{read_varint, write_varint, ValueError};
use std::collections::HashMap;

/// Magic bytes at the start of a serialized key dictionary.
pub const MAGIC: &[u8; 4] = b"RKDX";

/// Serialization format version for the dictionary.
pub const VERSION: u8 = 0x01;

/// Errors produced when (de)serializing a [`KeyDictionary`].
#[derive(Debug, PartialEq)]
pub enum KeyDictError {
    /// Buffer is too short to hold the header or a declared name.
    TruncatedData,
    /// First 4 bytes do not match `b"RKDX"`.
    BadMagic,
    /// Version byte is not [`VERSION`].
    UnsupportedVersion(u8),
    /// A field name is not valid UTF-8.
    InvalidName,
    /// A varint length prefix could not be decoded.
    Varint(ValueError),
}

impl From<ValueError> for KeyDictError {
    fn from(e: ValueError) -> Self {
        KeyDictError::Varint(e)
    }
}

impl std::fmt::Display for KeyDictError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TruncatedData => write!(f, "key dictionary: truncated data"),
            Self::BadMagic => write!(f, "key dictionary: bad magic bytes (expected RKDX)"),
            Self::UnsupportedVersion(v) => write!(f, "key dictionary: unsupported version {v}"),
            Self::InvalidName => write!(f, "key dictionary: name is not valid UTF-8"),
            Self::Varint(e) => write!(f, "key dictionary: varint error: {e}"),
        }
    }
}

impl std::error::Error for KeyDictError {}

/// An append-only field-name↔id catalogue for one document collection.
///
/// Ids are dense, assigned `0, 1, 2, …` in interning order, and never reused.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KeyDictionary {
    /// Field names indexed by their id (`names[id] == name`).
    names: Vec<String>,
    /// Reverse lookup: name → id.
    ids: HashMap<String, u32>,
}

impl KeyDictionary {
    /// Create an empty dictionary.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of interned field names.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Whether the dictionary holds no names.
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// Intern `name`, returning its (stable) id.
    ///
    /// If `name` is already present its existing id is returned and nothing is
    /// appended; otherwise a new id is assigned at the end of the catalogue.
    /// This is the **transactional append** used by the write path when it
    /// classifies a key as common.
    pub fn intern(&mut self, name: &str) -> u32 {
        if let Some(&id) = self.ids.get(name) {
            return id;
        }
        let id = self.names.len() as u32;
        self.names.push(name.to_string());
        self.ids.insert(name.to_string(), id);
        id
    }

    /// Look up the id for an already-interned `name`, if any.
    ///
    /// Unlike [`intern`](Self::intern) this never mutates the dictionary, so a
    /// caller can ask "is this key already common?" without appending.
    pub fn id_of(&self, name: &str) -> Option<u32> {
        self.ids.get(name).copied()
    }

    /// Resolve an id back to its field name, if the id is in range.
    pub fn name_of(&self, id: u32) -> Option<&str> {
        self.names.get(id as usize).map(String::as_str)
    }

    /// Serialize the dictionary into `out` for persistence in the collection
    /// catalogue.
    ///
    /// Names are written in id order, so a decode reconstructs the exact same
    /// id assignment.
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        write_varint(out, self.names.len() as u64);
        for name in &self.names {
            let bytes = name.as_bytes();
            write_varint(out, bytes.len() as u64);
            out.extend_from_slice(bytes);
        }
    }

    /// Deserialize a dictionary previously produced by [`encode`](Self::encode).
    pub fn decode(data: &[u8]) -> Result<Self, KeyDictError> {
        if data.len() < 5 {
            return Err(KeyDictError::TruncatedData);
        }
        if &data[0..4] != MAGIC.as_slice() {
            return Err(KeyDictError::BadMagic);
        }
        if data[4] != VERSION {
            return Err(KeyDictError::UnsupportedVersion(data[4]));
        }

        let mut cursor = 5;
        let (count, n) = read_varint(&data[cursor..])?;
        cursor += n;

        let mut dict = KeyDictionary::new();
        for _ in 0..count {
            let (len, n) = read_varint(&data[cursor..])?;
            cursor += n;
            let end = cursor
                .checked_add(len as usize)
                .ok_or(KeyDictError::TruncatedData)?;
            if end > data.len() {
                return Err(KeyDictError::TruncatedData);
            }
            let name =
                std::str::from_utf8(&data[cursor..end]).map_err(|_| KeyDictError::InvalidName)?;
            dict.intern(name);
            cursor = end;
        }

        Ok(dict)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_dictionary() {
        let dict = KeyDictionary::new();
        assert!(dict.is_empty());
        assert_eq!(dict.len(), 0);
        assert_eq!(dict.id_of("anything"), None);
        assert_eq!(dict.name_of(0), None);
    }

    #[test]
    fn intern_assigns_dense_ids_in_order() {
        let mut dict = KeyDictionary::new();
        assert_eq!(dict.intern("name"), 0);
        assert_eq!(dict.intern("email"), 1);
        assert_eq!(dict.intern("active"), 2);
        assert_eq!(dict.len(), 3);
    }

    #[test]
    fn intern_is_idempotent_and_append_only() {
        let mut dict = KeyDictionary::new();
        let first = dict.intern("status");
        dict.intern("createdAt");
        // Re-interning an existing key returns the same id and appends nothing.
        let again = dict.intern("status");
        assert_eq!(first, again);
        assert_eq!(dict.len(), 2);
    }

    #[test]
    fn id_of_does_not_mutate() {
        let mut dict = KeyDictionary::new();
        dict.intern("a");
        assert_eq!(dict.id_of("a"), Some(0));
        // A key that was never interned stays absent — id_of never appends.
        assert_eq!(dict.id_of("b"), None);
        assert_eq!(dict.len(), 1);
    }

    #[test]
    fn name_of_round_trips_ids() {
        let mut dict = KeyDictionary::new();
        dict.intern("first");
        dict.intern("second");
        assert_eq!(dict.name_of(0), Some("first"));
        assert_eq!(dict.name_of(1), Some("second"));
        assert_eq!(dict.name_of(2), None);
    }

    #[test]
    fn encode_decode_round_trip() {
        let mut dict = KeyDictionary::new();
        for k in ["id", "name", "email", "país", "🔑"] {
            dict.intern(k);
        }
        let mut buf = Vec::new();
        dict.encode(&mut buf);
        let restored = KeyDictionary::decode(&buf).expect("decode");
        assert_eq!(restored, dict);
        // Ids are preserved exactly.
        assert_eq!(restored.id_of("país"), Some(3));
        assert_eq!(restored.name_of(4), Some("🔑"));
    }

    #[test]
    fn empty_dictionary_round_trips() {
        let dict = KeyDictionary::new();
        let mut buf = Vec::new();
        dict.encode(&mut buf);
        let restored = KeyDictionary::decode(&buf).expect("decode");
        assert!(restored.is_empty());
    }

    #[test]
    fn decode_rejects_bad_magic_and_version() {
        let mut buf = Vec::new();
        KeyDictionary::new().encode(&mut buf);
        let mut bad = buf.clone();
        bad[0] = b'X';
        assert_eq!(KeyDictionary::decode(&bad), Err(KeyDictError::BadMagic));
        let mut bad = buf.clone();
        bad[4] = 0x99;
        assert_eq!(
            KeyDictionary::decode(&bad),
            Err(KeyDictError::UnsupportedVersion(0x99))
        );
    }

    #[test]
    fn decode_rejects_truncated() {
        assert_eq!(KeyDictionary::decode(&[]), Err(KeyDictError::TruncatedData));
        assert_eq!(
            KeyDictionary::decode(b"RKDX"),
            Err(KeyDictError::TruncatedData)
        );
    }
}
