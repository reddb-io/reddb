//! Reusable bloom filter header that any segment can embed.
//!
//! Segments (table pages, vector segments, timeseries chunks, graph
//! partitions) all benefit from the same question: "is key X *possibly* in
//! this segment?". Rather than reimplementing bloom wiring in every storage
//! engine, wrap [`crate::storage::primitives::split_block_bloom::SplitBlockBloom`]
//! here with a serialisation header and a small trait `HasBloom` for owners to
//! plug in.
//!
//! Layout on disk / inside a segment header is:
//!
//! ```text
//! [ u8  magic       ]
//! [ u8  reserved    ]   // zero
//! [ u32 inserted     ]   // monotonic counter, best-effort
//! [ bytes...          ]   // SplitBlockBloom::to_bytes()
//! ```

use crate::storage::primitives::split_block_bloom::SplitBlockBloom;

pub use reddb_file::BloomSegmentFrameError as BloomSegmentError;

/// Trait implemented by owners of a segment-level bloom filter.
///
/// Segments ask their owner (table, vector collection, timeseries chunk)
/// whether a key is definitely absent before walking their main structure.
pub trait HasBloom {
    /// Reference to the bloom filter attached to this segment, if any.
    fn bloom_segment(&self) -> Option<&BloomSegment>;

    /// Fast-path negative check. Returns `true` iff the bloom is present and
    /// reports the key as absent.
    fn definitely_absent(&self, key: &[u8]) -> bool {
        self.bloom_segment()
            .map(|b| b.definitely_absent(key))
            .unwrap_or(false)
    }
}

/// Owning bloom header that can be embedded in a segment.
pub struct BloomSegment {
    filter: SplitBlockBloom,
    inserted: u32,
}

impl std::fmt::Debug for BloomSegment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BloomSegment")
            .field("num_blocks", &self.filter.num_blocks())
            .field("inserted", &self.inserted)
            .finish()
    }
}

impl BloomSegment {
    /// Build a bloom sized for `expected` elements at the split-block
    /// primitive's fixed ~1% false-positive rate.
    pub fn with_capacity(expected: usize) -> Self {
        Self {
            filter: SplitBlockBloom::with_capacity(expected.max(16)),
            inserted: 0,
        }
    }

    /// Record `key` as possibly present.
    pub fn insert(&mut self, key: &[u8]) {
        self.filter.insert_bytes(key);
        self.inserted = self.inserted.saturating_add(1);
    }

    /// Might `key` be present? (May return a false positive, never a false
    /// negative.)
    pub fn contains(&self, key: &[u8]) -> bool {
        self.filter.probe_bytes(key)
    }

    /// Inverse of `contains` — the thing callers usually want.
    pub fn definitely_absent(&self, key: &[u8]) -> bool {
        !self.filter.probe_bytes(key)
    }

    /// Approximate current false-positive rate from word fill. Split-block
    /// bloom probes require all eight salted words to match, so this is a
    /// stats/debug estimate rather than a contract.
    pub fn estimated_fp_rate(&self) -> f64 {
        self.filter.fill_ratio().powi(8)
    }

    /// Number of elements recorded so far (best-effort).
    pub fn inserted_count(&self) -> u32 {
        self.inserted
    }

    /// Bytes used by the underlying split-block payload.
    pub fn byte_size(&self) -> usize {
        self.filter.byte_size()
    }

    /// Merge another bloom segment into this one. Both must have the same
    /// size and hash count. Returns `false` on mismatch.
    pub fn union_inplace(&mut self, other: &BloomSegment) -> bool {
        if self.filter.union_inplace(&other.filter) {
            self.inserted = self.inserted.saturating_add(other.inserted);
            true
        } else {
            false
        }
    }

    /// Serialise into the header layout documented at module level.
    pub fn encode(&self) -> Vec<u8> {
        reddb_file::encode_bloom_segment_frame(self.inserted, &self.filter.to_bytes())
    }

    /// Parse a previously encoded header. Returns a fresh `BloomSegment` and
    /// the number of bytes consumed.
    pub fn decode(bytes: &[u8]) -> Result<(Self, usize), BloomSegmentError> {
        let (inserted, bloom_blob, consumed) = reddb_file::decode_bloom_segment_frame(bytes)?;
        let filter =
            SplitBlockBloom::from_bytes(&bloom_blob).ok_or(BloomSegmentError::LengthMismatch)?;
        Ok((Self { filter, inserted }, consumed))
    }
}

/// Fluent builder that produces a `BloomSegment`.
pub struct BloomSegmentBuilder {
    expected: usize,
}

impl BloomSegmentBuilder {
    pub fn new() -> Self {
        Self { expected: 1024 }
    }

    pub fn expected(mut self, n: usize) -> Self {
        self.expected = n;
        self
    }

    pub fn build(self) -> BloomSegment {
        BloomSegment::with_capacity(self.expected)
    }
}

impl Default for BloomSegmentBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_query() {
        let mut seg = BloomSegment::with_capacity(1024);
        seg.insert(b"alpha");
        seg.insert(b"beta");

        assert!(seg.contains(b"alpha"));
        assert!(seg.contains(b"beta"));
        assert!(seg.definitely_absent(b"gamma") || seg.contains(b"gamma"));
        // No false negatives.
        assert!(!seg.definitely_absent(b"alpha"));
        assert_eq!(seg.inserted_count(), 2);
    }

    #[test]
    fn encode_decode_roundtrip() {
        let mut seg = BloomSegment::with_capacity(512);
        for i in 0..100 {
            seg.insert(format!("key{i}").as_bytes());
        }

        let bytes = seg.encode();
        let (restored, consumed) = BloomSegment::decode(&bytes).unwrap();
        assert_eq!(consumed, bytes.len());
        assert_eq!(restored.inserted_count(), 100);
        for i in 0..100 {
            assert!(restored.contains(format!("key{i}").as_bytes()));
        }
    }

    #[test]
    fn decode_rejects_bad_magic() {
        let mut bytes = BloomSegment::with_capacity(64).encode();
        bytes[0] = 0x00;
        assert_eq!(
            BloomSegment::decode(&bytes).unwrap_err(),
            BloomSegmentError::BadMagic
        );
    }

    #[test]
    fn decode_rejects_short_buffer() {
        let bytes = [reddb_file::BLOOM_SEGMENT_V2_MAGIC, 0, 0, 0];
        assert_eq!(
            BloomSegment::decode(&bytes).unwrap_err(),
            BloomSegmentError::TooShort
        );
    }

    #[test]
    fn decode_rejects_truncated_payload() {
        let mut bytes = BloomSegment::with_capacity(64).encode();
        bytes.truncate(bytes.len() - 1);
        assert_eq!(
            BloomSegment::decode(&bytes).unwrap_err(),
            BloomSegmentError::LengthMismatch
        );
    }

    #[test]
    fn union_merges_populations() {
        let mut a = BloomSegment::with_capacity(1024);
        let mut b = BloomSegment::with_capacity(1024);
        a.insert(b"one");
        b.insert(b"two");
        assert!(a.union_inplace(&b));
        assert!(a.contains(b"one"));
        assert!(a.contains(b"two"));
        assert_eq!(a.inserted_count(), 2);
    }

    #[test]
    fn union_rejects_incompatible() {
        let mut a = BloomSegment::with_capacity(1024);
        let b = BloomSegment::with_capacity(4096);
        assert!(!a.union_inplace(&b));
    }

    #[test]
    fn has_bloom_default_absent_when_none() {
        struct NoBloom;
        impl HasBloom for NoBloom {
            fn bloom_segment(&self) -> Option<&BloomSegment> {
                None
            }
        }
        let x = NoBloom;
        assert!(!x.definitely_absent(b"anything"));
    }
}
