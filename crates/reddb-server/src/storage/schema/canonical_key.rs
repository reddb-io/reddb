//! Re-export shim for canonical-key derivation.
//!
//! The canonical-key vocabulary — `CanonicalKey`, `CanonicalKeyFamily`, and
//! the `value_to_canonical_key` derivation that ordered secondary indexes key
//! on — is logical type vocabulary, so it was re-homed byte-faithfully into
//! the neutral keystone crate [`reddb_types`] (ADR 0052). This shim keeps the
//! historical `storage::schema::canonical_key` path resolving so existing
//! call-sites stay untouched.

pub use reddb_types::canonical_key::*;
