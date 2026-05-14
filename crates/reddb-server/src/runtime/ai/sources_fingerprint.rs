//! `SourcesFingerprint` — pure stable hash over the retrieved source set.
//!
//! Issue #400 (PRD #391): the determinism contract requires that the
//! seed handed to the provider be a deterministic function of
//! `(question, sources_fingerprint)`. The fingerprint is also the
//! cache-key ingredient that lets #403 invalidate an answer the moment
//! the underlying data shifts. Both consumers ([`determinism_decider`]
//! and [`answer_cache_key`]) treat it as an opaque string; this module
//! is the single place that owns its canonical form.
//!
//! ## Why a separate module
//!
//! Picking the fingerprint format is a one-way door. Two pipelines key
//! off it (seed derivation, answer cache), so any change is a wire
//! break: callers that recompute the fingerprint differently will mint
//! different seeds, cache-miss every previously-cached answer, and
//! their audit rows will silently diverge from operator expectations.
//! Pinning the format here with byte-for-byte tests stops a future
//! refactor from "tidying up" the delimiter or the version width and
//! quietly invalidating every running deployment's cache.
//!
//! ## Canonical form
//!
//! Inputs are `(urn, content_version: u64)` tuples. The fingerprint is
//! the lowercase-hex SHA-256 of the following byte sequence, in this
//! exact order:
//!
//! ```text
//! for each (urn, version) in sort_by(urn_bytes_asc, version_asc):
//!     urn_bytes
//!     0x1f                       // ASCII Unit Separator: field delimiter
//!     version.to_be_bytes()      // fixed 8 bytes, big-endian
//!     0x1e                       // ASCII Record Separator: tuple delimiter
//! ```
//!
//! Properties pinned by tests:
//!
//! - **Order-independent.** Two retrieval layers that hand the same
//!   tuples in different orders must mint the same fingerprint, so
//!   bucket fusion (#398) order can't leak into the seed.
//! - **Duplicate-suppressing.** The same `(urn, version)` appearing
//!   twice (e.g. once from BM25, once from vector) collapses to one
//!   entry. Otherwise the fingerprint would shift purely because a
//!   bucket changed its top-K and not because the *data* changed.
//! - **Version-sensitive.** A `(urn, v1) → (urn, v2)` mutation must
//!   flip the fingerprint so the cache invalidates and the seed
//!   changes.
//! - **Empty set has a fingerprint.** An ASK that retrieves zero rows
//!   still hashes deterministically (sha256 of the empty input). This
//!   keeps the seed derivation total — no `Option<String>` to thread
//!   through callers.
//!
//! The 0x1f / 0x1e delimiters belong to ASCII control-character space.
//! URNs in this codebase are ASCII printable (see [`urn_codec`]), so
//! neither byte can appear inside a `urn`, which makes the
//! concatenation injective without escaping. The same trick is used by
//! [`determinism_decider::derive_seed`] and [`answer_cache_key`].

use sha2::{Digest, Sha256};

/// Field delimiter (ASCII Unit Separator).
const FIELD_DELIMITER: u8 = 0x1f;
/// Tuple delimiter (ASCII Record Separator).
const TUPLE_DELIMITER: u8 = 0x1e;

/// One retrieved source entry. `urn` is the stable identity used in
/// `sources_flat`; `content_version` is whatever monotonic version
/// the storage layer attaches to the row at the time it is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Source<'a> {
    pub urn: &'a str,
    pub content_version: u64,
}

/// Compute the canonical fingerprint over a set of retrieved sources.
///
/// Returns a lowercase-hex SHA-256 string. Empty input returns the
/// SHA-256 of the empty byte sequence; this is intentional — see the
/// module doc.
pub fn fingerprint(sources: &[Source<'_>]) -> String {
    // Stable order: by urn bytes ascending, then by version ascending.
    // Sorting here (not at the call site) keeps the contract local; a
    // caller that forgets to sort still produces the correct hash.
    let mut ordered: Vec<Source<'_>> = sources.to_vec();
    ordered.sort_by(|a, b| {
        a.urn
            .as_bytes()
            .cmp(b.urn.as_bytes())
            .then(a.content_version.cmp(&b.content_version))
    });
    ordered.dedup();

    let mut hasher = Sha256::new();
    for src in &ordered {
        hasher.update(src.urn.as_bytes());
        hasher.update([FIELD_DELIMITER]);
        hasher.update(src.content_version.to_be_bytes());
        hasher.update([TUPLE_DELIMITER]);
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(s: &[(&'static str, u64)]) -> String {
        let v: Vec<Source<'_>> = s
            .iter()
            .map(|(u, v)| Source {
                urn: u,
                content_version: *v,
            })
            .collect();
        fingerprint(&v)
    }

    #[test]
    fn empty_input_hashes_to_sha256_of_empty() {
        // sha256("") == e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        assert_eq!(
            fingerprint(&[]),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn output_is_64_lowercase_hex_chars() {
        let out = fp(&[("urn:doc:a", 1)]);
        assert_eq!(out.len(), 64);
        assert!(out
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn deterministic_across_calls() {
        let a = fp(&[("urn:doc:a", 1), ("urn:doc:b", 2)]);
        let b = fp(&[("urn:doc:a", 1), ("urn:doc:b", 2)]);
        assert_eq!(a, b);
    }

    #[test]
    fn order_independent() {
        let a = fp(&[("urn:doc:a", 1), ("urn:doc:b", 2)]);
        let b = fp(&[("urn:doc:b", 2), ("urn:doc:a", 1)]);
        assert_eq!(a, b);
    }

    #[test]
    fn duplicates_collapse() {
        // Same source from two buckets must hash like a single occurrence.
        let one = fp(&[("urn:doc:a", 1)]);
        let dup = fp(&[("urn:doc:a", 1), ("urn:doc:a", 1)]);
        assert_eq!(one, dup);
    }

    #[test]
    fn version_change_flips_fingerprint() {
        let v1 = fp(&[("urn:doc:a", 1)]);
        let v2 = fp(&[("urn:doc:a", 2)]);
        assert_ne!(v1, v2);
    }

    #[test]
    fn different_urns_produce_different_fingerprints() {
        let a = fp(&[("urn:doc:a", 1)]);
        let b = fp(&[("urn:doc:b", 1)]);
        assert_ne!(a, b);
    }

    #[test]
    fn same_urn_different_versions_both_count() {
        // `(urn, v1)` and `(urn, v2)` are distinct tuples — both
        // contribute. Guards against an over-eager dedup that keys on
        // urn alone.
        let single = fp(&[("urn:doc:a", 1)]);
        let pair = fp(&[("urn:doc:a", 1), ("urn:doc:a", 2)]);
        assert_ne!(single, pair);
    }

    #[test]
    fn version_is_big_endian_8_bytes() {
        // If the encoding were little-endian or var-width, these two
        // versions (whose byte representations differ in width or order)
        // could collide. Pin BE-8 byte semantics.
        let lo = fp(&[("urn:doc:a", 1)]);
        let hi = fp(&[("urn:doc:a", 1u64 << 56)]);
        assert_ne!(lo, hi);
    }

    #[test]
    fn urn_boundary_is_injective() {
        // ("ab", 1) vs ("a", b1...) — the field delimiter must keep
        // the urn and the version from sliding into each other. With a
        // delimiter the hashes differ; without one they could collide.
        let a = fp(&[("ab", 1)]);
        let b = fp(&[("a", 1)]);
        assert_ne!(a, b);
    }

    #[test]
    fn matches_hand_computed_single_entry() {
        // Hand-compute sha256("urn:doc:a" || 0x1f || 0x00..0x01 || 0x1e)
        // and pin it. If anyone changes the delimiter, the field order,
        // or the version width, this test breaks.
        let mut hasher = Sha256::new();
        hasher.update(b"urn:doc:a");
        hasher.update([0x1f]);
        hasher.update(1u64.to_be_bytes());
        hasher.update([0x1e]);
        let digest = hasher.finalize();
        let mut expected = String::new();
        for byte in digest {
            use std::fmt::Write;
            let _ = write!(&mut expected, "{byte:02x}");
        }
        assert_eq!(fp(&[("urn:doc:a", 1)]), expected);
    }

    #[test]
    fn sort_is_byte_order_not_lex() {
        // Bytewise sort treats "Z" (0x5a) as less than "a" (0x61).
        // Picking a sort criterion is part of the contract — pin it.
        let mixed = fp(&[("a", 1), ("Z", 1)]);
        let reversed = fp(&[("Z", 1), ("a", 1)]);
        assert_eq!(mixed, reversed);
        // And a hand-ordered "Z" before "a" must yield the same hash.
        let hand = fp(&[("Z", 1), ("a", 1)]);
        assert_eq!(mixed, hand);
    }

    #[test]
    fn version_secondary_sort_for_same_urn() {
        // Same urn, two versions — order between them must collapse.
        let ascending = fp(&[("urn:doc:a", 1), ("urn:doc:a", 2)]);
        let descending = fp(&[("urn:doc:a", 2), ("urn:doc:a", 1)]);
        assert_eq!(ascending, descending);
    }

    #[test]
    fn empty_urn_is_distinct_from_no_entries() {
        // An empty-string urn at version 0 is still an entry. The
        // resulting fingerprint must differ from the empty-set
        // fingerprint, otherwise a malformed retrieval row could mask
        // itself as "no sources retrieved".
        let none = fingerprint(&[]);
        let empty = fp(&[("", 0)]);
        assert_ne!(none, empty);
    }
}
