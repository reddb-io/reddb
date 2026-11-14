# ADR 0086 — Native biometric records and extractor-agnostic matching

Status: proposed
Date: 2026-11-13

## Context

Applications need to preserve standardized fingerprint, face, and iris records
without reducing them to untyped blobs. They also need deterministic comparison
and indexed retrieval, but bundling computer-vision runtimes, provider adapters,
models, or model weights would make RedDB responsible for feature extraction
and materially expand the binary and attack surface.

Biometric interchange has two distinct layers. ISO/IEC and ANSI/NIST records
carry biometric samples and metadata. Matching implementations commonly consume
an algorithm-specific embedding or bit code that is not portable across
extractors. A database cannot safely compare two such artifacts merely because
their dimensions happen to agree.

CBEFF is a framework of registered patron formats, not a single self-identifying
binary container. ISO/IEC 39794 tagged binary records are ASN.1 and likewise do
not have one universal modality magic value.

## Decision

- Add native `FINGERPRINT`, `FACE`, and `IRIS` values. Each stores a stable
  format identifier and the original bounded standardized bytes. Standalone
  JPEG, PNG, and WSQ inputs are rejected.
- Add `FEATURE_VECTOR` and `BIT_SIGNATURE` for caller-supplied comparison
  artifacts. Both carry a 128-bit `space_id`; values from different spaces are
  incompatible even when their dimensions match.
- Accept the ISO/IEC 39794 and 19794, ANSI INCITS, and ANSI/NIST record families
  enumerated in the public type documentation. ISO/IEC 39794 is admitted only
  through an explicit typed format envelope and receives a bounded structural
  ASN.1 validation. CBEFF is admitted only through the concrete ANSI/NIST
  Type-99 carrier. These limits avoid making false claims about arbitrary
  patron formats or proprietary ASN.1 editions.
- For legacy self-identifying binary layouts, admit the explicitly parsed
  revisions listed in the public documentation. Shared ANSI/ISO magic values
  are resolved by validating both layouts; an unsupported revision or a record
  valid under more than one layout is rejected instead of guessed.
- Keep extraction, image region detection, liveness/PAD, biometric quality, and
  provider adapters outside RedDB. Face and iris comparison requires a caller-
  supplied feature vector or bit signature.
- Compare feature vectors with cosine similarity normalized from `[-1, 1]` to
  `[0, 1]`. Compare bit signatures with Hamming distance over bits valid in both
  masks. Incompatible spaces or widths are errors, never non-matches.
- Implement a deterministic minutiae matcher for supported ISO/IEC 19794-2 and
  ANSI INCITS 378 templates. It normalizes coordinates to micrometres, derives
  local rotation/translation-invariant descriptors, evaluates at most 32
  deterministic alignment anchors per view, applies one-to-one correspondence,
  and averages both comparison directions. Matcher version 3 is persisted in
  index contracts.
- Add HNSW, paginated four-bit Hamming-band postings, and local-descriptor
  minutiae postings for
  candidate generation. Exact comparison remains authoritative for reranking.
  `EXACT` bypasses candidate indexes. Equal scores are ordered by entity ID.
- Persist `space_id`, width, and index/matcher version in the catalog contract.
  An incompatible write is rejected before row publication. A stale contract
  is excluded from candidate planning and exact fallback remains available
  until rebuild.
- Add portable value tags 57 through 61, native-store tags 54 through 58, and
  RedWire parameter tags `0x0a` through `0x0e`. RedWire clients must negotiate
  `FEATURE_BIOMETRIC_PARAMS` (bit 3) before sending them.
- Add paged-file required capability bit 6. Readers that do not understand the
  values reject the file at open; files that never store the new values remain
  compatible with older readers.
- Never include raw biometric payloads or feature coordinates in validation
  errors, metadata functions, or operational logs.

## Bounded-resource estimate

The logical limits are 16 MiB per biometric record, 65,536 `f32` coordinates
per feature vector, and 1 MiB each for bit data and its effective mask. Parsers
validate lengths, counts, offsets, and multiplication/addition overflow before
allocating payload-dependent buffers.

Approximate portable payload cost, excluding surrounding row framing:

- biometric record: original bytes plus modality/format and a bounded length;
- feature vector: 16-byte space ID, 4-byte dimension, and `4 * dimension` bytes;
- bit signature: 16-byte space ID, 4-byte length, and `2 * length` bytes for
  bits plus the effective mask.

Cosine comparison is `O(d)`. Masked Hamming comparison is `O(b)`. Local
minutiae descriptor construction sorts neighbours and is `O(m² log m)` in the
current deliberately simple implementation. Exact aligned matching is bounded
by 32 anchors per side and uses a spatial grid for correspondence rather than a
full scan for every transformed point.

The feature index stores approximately `4d` bytes per vector plus HNSW graph
links and allocator overhead. Hamming and minutiae indexes retain exact values
for reranking plus reconstructible postings. Hamming postings use four-bit
bands and 64-ID pages. Scores at or below 0.75 use exact fallback because a
four-bit band cannot guarantee a zero-mismatch band there. Signatures longer
than 4 KiB also use exact fallback rather than materializing millions of band
keys. The indexes are secondary state and can be rebuilt from table data.

A Windows debug-profile qualification run with 512 entries produced the
following non-release receipt: feature/HNSW build 677,855 us, query 241 us,
resident estimate 422,564 bytes; masked Hamming build 17,860 us, query 1,027
us, resident estimate 806,052 bytes; minutiae build 6,981 us, query 1,412 us,
resident estimate 89,613 bytes. These figures are regression baselines for the
same host and profile, not throughput claims. Criterion benchmarks separately
cover codec decode, 1:1 comparison, descriptor construction, and exact 1:N
matching.

No production dependency, native library, model, weight, or packaged asset is
added. The release gate records executable size before and after this change;
unexpected growth is a blocker even if dependency count is unchanged.

## Consequences

- RedDB can validate, preserve, compare, index, replicate, back up, and version
  biometric values without becoming a computer-vision product.
- `space_id` is part of correctness, not optional metadata. Applications must
  change it whenever extractor semantics change.
- Some records are valid for storage but not for matching. In particular,
  image-only fingerprints cannot enter a `MINUTIAE` index.
- Schema-complete validation for every ISO/IEC 39794 edition or another CBEFF
  patron format requires a future, separately reviewed parser. Structural ASN.1
  validation must not be described as full conformance validation.
- The addition is minor-version work because it adds SQL, wire tags, persistent
  value tags, and a required file capability without changing existing values.
