//! L2 Blob Compressor.
//!
//! Stateless, deep module encapsulating compression of blob payloads spilled to
//! the L2 (on-disk) tier of [`BlobCache`]. The purpose is to shrink durable blob
//! footprints without harming hot-path latency or wasting CPU on payloads that
//! are already incompressible.
//!
//! # Design
//!
//! The module is intentionally small and side-effect free:
//!
//! - All operations are static — no internal state, no allocator pinning, no
//!   threading concerns. Inputs are `&[u8]` slices, outputs are owned `Vec<u8>`.
//! - Compression is best-effort: when the input is small, when the content type
//!   already represents a compressed media format, or when the `zstd` output
//!   fails to shrink the input meaningfully, the original bytes are returned
//!   inside [`Compressed::Raw`] and no encode is performed (or its result is
//!   discarded).
//! - The compressed variant carries the original byte length so decompression
//!   can pre-allocate exactly and verify the encoded length on decode.
//!
//! Wiring into [`BlobCache`] L2 `put`/`get` is performed in a follow-up slice;
//! this module is additive and has no callers in this commit.

use std::fmt;

/// Default `zstd` compression level — favours encode speed over ratio. The L2
/// tier is meant to amplify capacity, not to be the smallest possible store.
pub const DEFAULT_ZSTD_LEVEL: i32 = 1;

/// Default minimum payload size eligible for compression. Sub-kilobyte payloads
/// rarely benefit and the framing overhead can exceed any savings.
pub const DEFAULT_MIN_BYTES: usize = 1024;

/// Default cutoff ratio above which the compressed bytes are discarded.
/// `0.95` means we require at least a 5% reduction to keep the encoded form.
pub const DEFAULT_MAX_RATIO: f64 = 0.95;

/// Configuration knobs for [`L2BlobCompressor::compress`].
#[derive(Clone, Copy, Debug)]
pub struct CompressOpts {
    /// `zstd` compression level. Higher is slower / smaller.
    pub level: i32,
    /// Inputs strictly smaller than this byte count are returned raw.
    pub min_bytes: usize,
    /// Skip the encoded form when `compressed.len() >= input.len() * max_ratio`.
    pub max_ratio: f64,
}

impl Default for CompressOpts {
    fn default() -> Self {
        Self {
            level: DEFAULT_ZSTD_LEVEL,
            min_bytes: DEFAULT_MIN_BYTES,
            max_ratio: DEFAULT_MAX_RATIO,
        }
    }
}

/// Storage-ready representation of a blob payload after the compressor has
/// inspected it. `Raw` is byte-equivalent to the input; `Zstd` carries an
/// encoded payload plus the original byte length for verification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Compressed {
    /// The bytes were left untouched (skip rule fired or no shrinkage).
    Raw(Vec<u8>),
    /// `zstd`-encoded payload. `original_len` is the byte length the decoded
    /// stream must produce.
    Zstd { bytes: Vec<u8>, original_len: u32 },
}

impl Compressed {
    /// Length of the on-disk payload (encoded bytes for `Zstd`, raw bytes for
    /// `Raw`). Useful for L2 budget accounting.
    pub fn stored_len(&self) -> usize {
        match self {
            Self::Raw(b) => b.len(),
            Self::Zstd { bytes, .. } => bytes.len(),
        }
    }

    /// Length the bytes occupy after decompression — equal to the original
    /// input size in both variants.
    pub fn original_len(&self) -> usize {
        match self {
            Self::Raw(b) => b.len(),
            Self::Zstd { original_len, .. } => *original_len as usize,
        }
    }

    /// `true` when the payload is `zstd`-encoded.
    pub fn is_compressed(&self) -> bool {
        matches!(self, Self::Zstd { .. })
    }
}

/// Errors produced by the compressor.
#[derive(Debug)]
pub enum CompressError {
    /// `zstd` failed to encode the payload. The inner string is the encoder
    /// error rendered for diagnostics.
    ZstdEncode(String),
    /// `zstd` failed to decode the payload, or the decoded length did not
    /// match the recorded `original_len`.
    ZstdDecode(String),
    /// A persisted [`Compressed`] value was tagged with an unknown format.
    /// Reserved for forward-compatible callers; not produced by this module.
    UnknownFormat,
    /// Input payload exceeds `u32::MAX` bytes — the original-length field
    /// cannot represent it.
    OversizeOriginal(usize),
}

impl fmt::Display for CompressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZstdEncode(msg) => write!(f, "zstd encode failed: {msg}"),
            Self::ZstdDecode(msg) => write!(f, "zstd decode failed: {msg}"),
            Self::UnknownFormat => write!(f, "unknown compressed format"),
            Self::OversizeOriginal(n) => {
                write!(
                    f,
                    "payload of {n} bytes exceeds u32::MAX original-length cap"
                )
            }
        }
    }
}

impl std::error::Error for CompressError {}

/// Stateless compressor for L2 blob payloads.
///
/// All methods are associated functions; the type carries no data and is
/// unconstructible at runtime. It exists purely as a namespace and to keep the
/// public surface in one place.
pub struct L2BlobCompressor;

impl L2BlobCompressor {
    /// Compress `bytes`, honouring the skip rules in [`CompressOpts`] and the
    /// `content_type` hint.
    ///
    /// # Skip rules
    ///
    /// 1. `bytes.len() < opts.min_bytes` → returns [`Compressed::Raw`].
    /// 2. `content_type` matches a pre-compressed media bucket
    ///    (see [`is_precompressed_media`]) → returns [`Compressed::Raw`].
    /// 3. The encoded payload would be `>= bytes.len() * opts.max_ratio` →
    ///    returns [`Compressed::Raw`].
    ///
    /// # Errors
    ///
    /// - [`CompressError::OversizeOriginal`] if `bytes.len() > u32::MAX`.
    /// - [`CompressError::ZstdEncode`] if the underlying encoder fails.
    pub fn compress(
        bytes: &[u8],
        content_type: Option<&str>,
        opts: &CompressOpts,
    ) -> Result<Compressed, CompressError> {
        // Reject payloads that cannot be represented in the on-disk header.
        if bytes.len() > u32::MAX as usize {
            return Err(CompressError::OversizeOriginal(bytes.len()));
        }

        // Skip rule 1: small payload — framing overhead dominates.
        if bytes.len() < opts.min_bytes {
            return Ok(Compressed::Raw(bytes.to_vec()));
        }

        // Skip rule 2: content type already represents compressed media.
        if let Some(ct) = content_type {
            if is_precompressed_media(ct) {
                return Ok(Compressed::Raw(bytes.to_vec()));
            }
        }

        // Encode via zstd. Errors here are surfaced — the caller can choose to
        // fall back to a raw write rather than failing the L2 put outright.
        let encoded = zstd::stream::encode_all(bytes, opts.level)
            .map_err(|e| CompressError::ZstdEncode(e.to_string()))?;

        // Skip rule 3: no meaningful shrinkage. Comparing as f64 keeps the
        // ratio knob expressive (e.g. 0.5 to require 2x reduction).
        let cutoff = (bytes.len() as f64) * opts.max_ratio;
        if (encoded.len() as f64) >= cutoff {
            return Ok(Compressed::Raw(bytes.to_vec()));
        }

        Ok(Compressed::Zstd {
            bytes: encoded,
            original_len: bytes.len() as u32,
        })
    }

    /// Decompress a previously-stored [`Compressed`] payload back to the
    /// original byte slice.
    ///
    /// # Errors
    ///
    /// - [`CompressError::ZstdDecode`] if the encoded bytes are malformed or if
    ///   the decoded length does not match the recorded `original_len`.
    pub fn decompress(c: &Compressed) -> Result<Vec<u8>, CompressError> {
        match c {
            Compressed::Raw(b) => Ok(b.clone()),
            Compressed::Zstd {
                bytes,
                original_len,
            } => {
                let mut out: Vec<u8> = Vec::with_capacity(*original_len as usize);
                let written = {
                    let mut decoder = zstd::stream::Decoder::new(bytes.as_slice())
                        .map_err(|e| CompressError::ZstdDecode(e.to_string()))?;
                    std::io::copy(&mut decoder, &mut out)
                        .map_err(|e| CompressError::ZstdDecode(e.to_string()))?
                };
                if written as usize != *original_len as usize {
                    return Err(CompressError::ZstdDecode(format!(
                        "decoded {written} bytes, expected {original_len}"
                    )));
                }
                Ok(out)
            }
        }
    }
}

/// Returns `true` when the supplied MIME-style content type names a media
/// format that is already compressed and therefore not worth re-encoding.
///
/// Exceptions:
///
/// - `image/svg+xml` is treated as XML text and remains eligible.
/// - `audio/wav` and `audio/x-wav` are uncompressed PCM and remain eligible.
fn is_precompressed_media(content_type: &str) -> bool {
    // Strip parameters such as `;charset=utf-8` and normalise case.
    let head = content_type.split(';').next().unwrap_or("").trim();
    let lower = head.to_ascii_lowercase();

    if let Some(rest) = lower.strip_prefix("image/") {
        // SVG is text-based and benefits substantially from zstd.
        return rest != "svg+xml";
    }
    if lower.starts_with("video/") {
        return true;
    }
    if let Some(rest) = lower.strip_prefix("audio/") {
        // PCM WAV is uncompressed — let zstd handle it.
        return !matches!(rest, "wav" | "x-wav");
    }

    matches!(
        lower.as_str(),
        "application/zip"
            | "application/gzip"
            | "application/x-brotli"
            | "application/x-zstd"
            | "application/octet-stream"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tiny pseudo-random byte generator. We deliberately avoid pulling in a
    /// crate dependency for tests — a linear congruential generator gives us
    /// reproducible "random-looking" bytes that round-trip exactly.
    fn pseudo_random(seed: u64, len: usize) -> Vec<u8> {
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            out.push((state >> 33) as u8);
        }
        out
    }

    fn lorem_4kb() -> Vec<u8> {
        let unit = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
                     Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. \
                     Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris \
                     nisi ut aliquip ex ea commodo consequat. ";
        let mut out = Vec::with_capacity(4096 + unit.len());
        while out.len() < 4096 {
            out.extend_from_slice(unit);
        }
        out.truncate(4096);
        out
    }

    #[test]
    fn round_trip_property_across_sizes() {
        // Sample sizes from 0 to 16384 across a spread of seeds. Every output
        // must decompress back to the exact input regardless of which branch
        // (Raw / Zstd) the compressor chose.
        let opts = CompressOpts::default();
        let sizes = [
            0usize, 1, 16, 64, 255, 511, 1023, 1024, 1025, 2048, 4096, 8192, 12345, 16384,
        ];
        for (i, &len) in sizes.iter().enumerate() {
            let input = pseudo_random(0xDEAD_BEEF ^ (i as u64), len);
            let compressed = L2BlobCompressor::compress(&input, None, &opts)
                .expect("compress should not fail on in-memory input");
            let decoded = L2BlobCompressor::decompress(&compressed)
                .expect("decompress should not fail on freshly-encoded input");
            assert_eq!(decoded, input, "round-trip mismatch at len={len}");
            assert_eq!(compressed.original_len(), input.len());
        }
    }

    #[test]
    fn text_payload_shrinks_at_least_thirty_percent() {
        let input = lorem_4kb();
        let opts = CompressOpts::default();
        let compressed = L2BlobCompressor::compress(&input, Some("text/plain"), &opts)
            .expect("compress text payload");
        match compressed {
            Compressed::Zstd {
                bytes,
                original_len,
            } => {
                assert_eq!(original_len as usize, input.len());
                let ratio = bytes.len() as f64 / input.len() as f64;
                assert!(
                    ratio <= 0.70,
                    "expected >=30% reduction, got ratio {ratio} ({}/{})",
                    bytes.len(),
                    input.len()
                );
            }
            other => panic!("expected Zstd variant for repetitive text, got {other:?}"),
        }
    }

    #[test]
    fn tiny_payload_returns_raw() {
        let input = vec![0xABu8; 64]; // below default min_bytes (1024)
        let opts = CompressOpts::default();
        let out = L2BlobCompressor::compress(&input, None, &opts).unwrap();
        match out {
            Compressed::Raw(bytes) => assert_eq!(bytes, input),
            other => panic!("expected Raw for tiny payload, got {other:?}"),
        }
    }

    #[test]
    fn image_png_content_type_returns_raw_even_when_large() {
        // Highly compressible payload, but the content-type rule short-circuits.
        let input = vec![0u8; 8 * 1024];
        let opts = CompressOpts::default();
        let out = L2BlobCompressor::compress(&input, Some("image/png"), &opts).unwrap();
        assert!(matches!(out, Compressed::Raw(_)), "PNG must be Raw");
    }

    #[test]
    fn image_svg_is_compressed_as_exception() {
        // SVG is XML text and should be eligible for compression.
        let mut input = Vec::new();
        let chunk =
            b"<svg xmlns='http://www.w3.org/2000/svg'><rect width='10' height='10'/></svg>\n";
        while input.len() < 4096 {
            input.extend_from_slice(chunk);
        }
        let opts = CompressOpts::default();
        let out = L2BlobCompressor::compress(&input, Some("image/svg+xml"), &opts).unwrap();
        assert!(out.is_compressed(), "image/svg+xml should be compressed");
    }

    #[test]
    fn high_entropy_payload_returns_raw_via_max_ratio_gate() {
        // Pseudo-random bytes have ~no redundancy; zstd cannot meaningfully
        // shrink them and the max_ratio gate must reject the encoded form.
        let input = pseudo_random(0xCAFE_F00D, 8 * 1024);
        let opts = CompressOpts::default();
        let out = L2BlobCompressor::compress(&input, None, &opts).unwrap();
        match out {
            Compressed::Raw(bytes) => assert_eq!(bytes, input),
            Compressed::Zstd { bytes, .. } => {
                panic!(
                    "high-entropy input was kept as Zstd ({} bytes vs {} original) — \
                     max_ratio gate failed",
                    bytes.len(),
                    input.len()
                );
            }
        }
    }

    #[test]
    fn malformed_zstd_bytes_yield_decode_error() {
        let bogus = Compressed::Zstd {
            bytes: vec![0x00, 0x01, 0x02, 0x03, 0xFF, 0xFE, 0xFD, 0xFC],
            original_len: 4096,
        };
        let err = L2BlobCompressor::decompress(&bogus).expect_err("must fail to decode");
        assert!(
            matches!(err, CompressError::ZstdDecode(_)),
            "expected ZstdDecode, got {err:?}"
        );
    }

    #[test]
    fn decoded_length_mismatch_yields_decode_error() {
        // Encode 1024 bytes but lie about the original length so the post-decode
        // verification step trips.
        let input = lorem_4kb();
        let truthful =
            L2BlobCompressor::compress(&input, Some("text/plain"), &CompressOpts::default())
                .unwrap();
        let lying = match truthful {
            Compressed::Zstd { bytes, .. } => Compressed::Zstd {
                bytes,
                original_len: (input.len() as u32) + 1,
            },
            other => panic!("expected Zstd, got {other:?}"),
        };
        let err = L2BlobCompressor::decompress(&lying).expect_err("must fail length check");
        assert!(matches!(err, CompressError::ZstdDecode(_)));
    }

    /// Synthetic oversize check — we cannot allocate 4 GiB in a unit test, so
    /// we forge a slice header pointing at a tiny backing buffer but reporting
    /// a length larger than `u32::MAX`. The compressor must reject by length
    /// inspection alone, before touching the bytes.
    ///
    /// SAFETY: we never read from the synthetic slice. `compress` only inspects
    /// `bytes.len()` on the early-exit path that this test exercises.
    #[test]
    fn oversize_input_returns_oversize_error() {
        let backing = [0u8; 16];
        let fake_len = (u32::MAX as usize) + 1;
        // Build a slice with an inflated length that we promise never to read.
        let oversized: &[u8] = unsafe { std::slice::from_raw_parts(backing.as_ptr(), fake_len) };
        let err = L2BlobCompressor::compress(oversized, None, &CompressOpts::default())
            .expect_err("must reject oversize input");
        match err {
            CompressError::OversizeOriginal(n) => assert_eq!(n, fake_len),
            other => panic!("expected OversizeOriginal, got {other:?}"),
        }
    }

    #[test]
    fn precompressed_media_classifier_handles_known_buckets() {
        assert!(is_precompressed_media("image/png"));
        assert!(is_precompressed_media("image/jpeg"));
        assert!(!is_precompressed_media("image/svg+xml"));
        assert!(is_precompressed_media("video/mp4"));
        assert!(is_precompressed_media("video/webm"));
        assert!(is_precompressed_media("audio/mpeg"));
        assert!(!is_precompressed_media("audio/wav"));
        assert!(!is_precompressed_media("audio/x-wav"));
        assert!(is_precompressed_media("application/zip"));
        assert!(is_precompressed_media("application/gzip"));
        assert!(is_precompressed_media("application/x-brotli"));
        assert!(is_precompressed_media("application/x-zstd"));
        assert!(is_precompressed_media("application/octet-stream"));
        assert!(!is_precompressed_media("text/plain"));
        assert!(!is_precompressed_media("application/json"));
        // Parameter handling + casing.
        assert!(is_precompressed_media("Image/PNG; foo=bar"));
        assert!(!is_precompressed_media("Text/Plain; charset=utf-8"));
    }
}
