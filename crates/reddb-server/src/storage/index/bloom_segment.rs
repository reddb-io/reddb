//! Reusable bloom filter header that any segment can embed.
//!
//! Segments (table pages, vector segments, timeseries chunks, graph
//! partitions) all benefit from the same question: "is key X *possibly* in
//! this segment?". Rather than reimplementing bloom wiring in every storage
//! engine, wrap [`crate::storage::primitives::BloomFilter`] here with a
//! serialisation header and a small trait `HasBloom` for owners to plug in.
//!
//! Layout on disk / inside a segment header is:
//!
//! ```text
//! [ u8  magic = 0xBF ]
//! [ u8  num_hashes   ]
//! [ u32 bit_size     ]   // big-endian
//! [ u32 inserted     ]   // monotonic counter, best-effort
//! [ bytes...          ]   // bit array
//! ```
//!
//! Readers that don't care about bloom can skip `4 + (bit_size + 7) / 8`
//! bytes after the 10-byte header.

use crate::storage::primitives::BloomFilter;

const BLOOM_SEGMENT_MAGIC: u8 = 0xBF;
const HEADER_LEN: usize = 1 + 1 + 4 + 4; // magic + hashes + bit_size + inserted

/// Error returned when parsing a bloom segment header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BloomSegmentError {
    /// Byte slice was shorter than the fixed header.
    TooShort,
    /// Magic byte did not match [`BLOOM_SEGMENT_MAGIC`].
    BadMagic,
    /// Declared bit size did not match payload length.
    LengthMismatch,
}

impl std::fmt::Display for BloomSegmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BloomSegmentError::TooShort => write!(f, "bloom header too short"),
            BloomSegmentError::BadMagic => write!(f, "bloom header magic mismatch"),
            BloomSegmentError::LengthMismatch => write!(f, "bloom header length mismatch"),
        }
    }
}

impl std::error::Error for BloomSegmentError {}

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
    filter: BloomFilter,
    inserted: u32,
}

impl std::fmt::Debug for BloomSegment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BloomSegment")
            .field("bit_size", &self.filter.bit_size())
            .field("num_hashes", &self.filter.num_hashes())
            .field("inserted", &self.inserted)
            .finish()
    }
}

impl BloomSegment {
    /// Build a bloom sized for `expected` elements at a 1% false-positive
    /// rate. Cheap — allocates `~9.6 * expected / 8` bytes.
    pub fn with_capacity(expected: usize) -> Self {
        Self {
            filter: BloomFilter::with_capacity(expected.max(16), 0.01),
            inserted: 0,
        }
    }

    /// Custom false-positive rate.
    pub fn with_rate(expected: usize, fp_rate: f64) -> Self {
        Self {
            filter: BloomFilter::with_capacity(expected.max(16), fp_rate),
            inserted: 0,
        }
    }

    /// Record `key` as possibly present.
    pub fn insert(&mut self, key: &[u8]) {
        self.filter.insert(key);
        self.inserted = self.inserted.saturating_add(1);
    }

    /// Might `key` be present? (May return a false positive, never a false
    /// negative.)
    pub fn contains(&self, key: &[u8]) -> bool {
        self.filter.contains(key)
    }

    /// Inverse of `contains` — the thing callers usually want.
    pub fn definitely_absent(&self, key: &[u8]) -> bool {
        !self.filter.contains(key)
    }

    /// Estimated current false-positive rate given the number of insertions.
    pub fn estimated_fp_rate(&self) -> f64 {
        self.filter.estimate_fp_rate(self.inserted as usize)
    }

    /// Number of elements recorded so far (best-effort).
    pub fn inserted_count(&self) -> u32 {
        self.inserted
    }

    /// Access the underlying bloom filter (e.g. to pass to
    /// [`crate::storage::index::IndexBase::bloom`]).
    pub fn filter(&self) -> &BloomFilter {
        &self.filter
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
        let bits = self.filter.as_bytes();
        let bit_size = self.filter.bit_size();
        let mut out = Vec::with_capacity(HEADER_LEN + bits.len());
        out.push(BLOOM_SEGMENT_MAGIC);
        out.push(self.filter.num_hashes());
        out.extend_from_slice(&bit_size.to_be_bytes());
        out.extend_from_slice(&self.inserted.to_be_bytes());
        out.extend_from_slice(bits);
        out
    }

    /// Parse a previously encoded header. Returns a fresh `BloomSegment` and
    /// the number of bytes consumed.
    pub fn decode(bytes: &[u8]) -> Result<(Self, usize), BloomSegmentError> {
        if bytes.len() < HEADER_LEN {
            return Err(BloomSegmentError::TooShort);
        }
        if bytes[0] != BLOOM_SEGMENT_MAGIC {
            return Err(BloomSegmentError::BadMagic);
        }
        let num_hashes = bytes[1];
        let bit_size = u32::from_be_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]);
        let inserted = u32::from_be_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]);
        let byte_len = (bit_size as usize).div_ceil(8);
        let total = HEADER_LEN + byte_len;
        if bytes.len() < total {
            return Err(BloomSegmentError::LengthMismatch);
        }
        let filter = BloomFilter::from_bytes_with_size(
            bytes[HEADER_LEN..total].to_vec(),
            num_hashes,
            bit_size,
        );
        Ok((Self { filter, inserted }, total))
    }
}

/// Fluent builder that mirrors `BloomFilterBuilder` but produces a
/// `BloomSegment`.
pub struct BloomSegmentBuilder {
    expected: usize,
    fp_rate: f64,
}

impl BloomSegmentBuilder {
    pub fn new() -> Self {
        Self {
            expected: 1024,
            fp_rate: 0.01,
        }
    }

    pub fn expected(mut self, n: usize) -> Self {
        self.expected = n;
        self
    }

    pub fn false_positive_rate(mut self, rate: f64) -> Self {
        self.fp_rate = rate;
        self
    }

    pub fn build(self) -> BloomSegment {
        BloomSegment::with_rate(self.expected, self.fp_rate)
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
        let bytes = [0xBF, 3, 0, 0];
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
        let mut a = BloomSegment::with_rate(1024, 0.01);
        let mut b = BloomSegment::with_rate(1024, 0.01);
        a.insert(b"one");
        b.insert(b"two");
        assert!(a.union_inplace(&b));
        assert!(a.contains(b"one"));
        assert!(a.contains(b"two"));
        assert_eq!(a.inserted_count(), 2);
    }

    #[test]
    fn union_rejects_incompatible() {
        let mut a = BloomSegment::with_rate(1024, 0.01);
        let b = BloomSegment::with_rate(4096, 0.01);
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
