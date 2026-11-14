# Biometric Records and Comparison Artifacts

RedDB stores standardized biometric records without extracting features from
images. It does not run computer vision, liveness/PAD, quality assessment, or
provider-specific models.

The six native types are:

| Type | Purpose | Comparison |
|:--|:--|:--|
| `FINGERPRINT` | Standardized fingerprint record | RedDB minutiae matcher when the record contains a supported minutiae template |
| `FACE` | Standardized face record | Storage only |
| `IRIS` | Standardized iris record | Storage only |
| `FEATURE_VECTOR` | Caller-supplied features in an identified feature space | Normalized cosine score in `0..=1` |
| `BIT_SIGNATURE` | Caller-supplied bits and optional validity mask in an identified feature space | Masked Hamming score in `0..=1` |
| `LANDMARK_SET` | Caller-supplied 2D or 3D bilateral geometry in an identified coordinate space | Deterministic symmetry score in `0..=1` |

Face and iris matching therefore uses a `FEATURE_VECTOR` or `BIT_SIGNATURE`
produced by the application. RedDB never claims that those artifacts are ISO
records or that they were derived from a stored sample.

## Supported record envelopes

`FINGERPRINT`, `FACE`, and `IRIS` preserve the original standardized bytes in
a versioned internal envelope. RedDB does not re-encode an embedded image or
invent missing metadata.

- Fingerprint: ISO/IEC 39794-2 and -4, ISO/IEC 19794-2 and -4, ANSI INCITS 378
  and 381, ANSI/NIST-ITL Type-4 and Type-9, and a CBEFF record carried by
  ANSI/NIST-ITL Type-99.
- Face: ISO/IEC 39794-5, ISO/IEC 19794-5, ANSI/NIST-ITL Type-10, and a CBEFF
  record carried by Type-99.
- Iris: ISO/IEC 39794-6, ISO/IEC 19794-6, ANSI/NIST-ITL Type-17, and a CBEFF
  record carried by Type-99.

Standalone JPEG, PNG, and WSQ blobs are rejected. Such image encodings remain
valid when they are inside a supported biometric record.

For the legacy binary families, the currently admitted layouts are ISO/IEC
19794-2:2005, ISO/IEC 19794-4:2005, ISO/IEC 19794-5:2005, ISO/IEC
19794-6:2005 and :2011, ANSI INCITS 378-2004/2009, and ANSI INCITS 381-2004.
The parser validates the declared length, nested record counts and lengths,
dimensions, and bounded offsets before accepting a record. A different binary
revision is rejected instead of being guessed from the four-byte magic value.

Legacy binary records with self-identifying headers and single matching
ANSI/NIST records are detected automatically. ISO/IEC 39794 is ASN.1 and does
not carry one universal modality header, so it must arrive in a typed transport
envelope that names the format. RedDB performs a bounded structural ASN.1 walk;
it does not currently claim full schema validation of every 39794 edition.

CBEFF is a framework with registered patron formats rather than one universal
byte layout. RedDB consequently accepts only the concrete ANSI/NIST Type-99
carrier that it can identify and validate. It does not guess the modality of an
arbitrary CBEFF blob.

For a transaction containing several ANSI/NIST records, select the one to
store explicitly. Occurrences are one-based:

```sql
FINGERPRINT(ANSI_NIST_RECORD($1, 9, 1))
FACE(ANSI_NIST_RECORD($1, 10, 1))
IRIS(ANSI_NIST_RECORD($1, 17, 1))
```

## Construction and inspection

```sql
FINGERPRINT($1)
FACE($1)
IRIS($1)
FEATURE_VECTOR($space_uuid, $vector)
BIT_SIGNATURE($space_uuid, $bits)
BIT_SIGNATURE($space_uuid, $bits, $mask)
LANDMARK_SET($space_uuid, $geometry)
```

`space_id` identifies the algorithm, version, preprocessing, and feature
semantics chosen by the caller. Changing any of those should use a new
`space_id`. A feature vector must be finite, non-empty, and non-zero. A bit
signature and mask must have the same non-zero length, and a mask must expose
at least one valid bit.

Landmark geometry is a canonical JSON object with `dimensions` equal to 2 or
3, at least two `left`/`right` pairs, and an optional `midline` array. It has
no weights: every bilateral pair and midline point contributes uniformly.
Coordinates must be finite and representable as `f32`; RedDB performs the
calculation in `f64`. A value may contain at most 4,096 points in total.

```json
{
  "dimensions": 2,
  "pairs": [
    { "left": [10.0, 20.0], "right": [30.0, 20.0] },
    { "left": [14.0, 30.0], "right": [26.0, 30.0] }
  ],
  "midline": [[20.0, 36.0]]
}
```

Inspection returns metadata only:

```sql
SELECT BIOMETRIC_FORMAT(fingerprint), BIOMETRIC_INFO(fingerprint)
FROM identities
```

`BIOMETRIC_INFO` contains modality, format, and byte length; it does not expose
the biometric payload.

## Comparison

```sql
FEATURE_COMPARE(a, b)
FEATURE_MATCH(a, b, 0.80)
BIT_SIGNATURE_COMPARE(a, b)
BIT_SIGNATURE_MATCH(a, b, 0.80)
FINGERPRINT_COMPARE(a, b)
FINGERPRINT_MATCH(a, b, 0.80)
```

Scores are deterministic and bounded to `0..=1`. Match thresholds must be
finite and in that same range. Incompatible `space_id`, dimensions, bit
lengths, masks with no common valid bits, fingerprint positions, or fingerprint
record kinds return an error instead of `false`.

The built-in fingerprint matcher works only with minutiae templates from
ISO/IEC 19794-2 and ANSI INCITS 378. Image-only fingerprint records can be
stored but cannot be compared or added to a `MINUTIAE` index. The matcher
normalizes coordinates to physical resolution, uses rotation/translation
invariant local descriptors for candidate generation, performs deterministic
alignment and one-to-one matching, and symmetrically normalizes the score.

### Landmark symmetry

```sql
LANDMARK_SYMMETRY(landmarks)
LANDMARK_IS_SYMMETRIC(landmarks, 0.95)
FACE_LANDMARKS(face)
```

The symmetry function fits a line in 2D or a plane in 3D from the oriented
bilateral vectors, pair midpoints, and midline points. It reflects each left
point, computes the RMSE to the corresponding right point plus each midline
distance, and normalizes the error by the RMS radius of the full set:

```text
score = clamp(1 - symmetry_rmse / landmark_scale, 0, 1)
```

The score is deterministic, equals `1` for exactly symmetric geometry, and is
invariant to translation, rotation, uniform scale, and swapping every left and
right point. Degenerate geometry, an undefined axis or plane, and a non-finite
threshold outside `0..=1` are errors. This release intentionally has no
`LANDMARK_COMPARE` function and no landmark index.

`FACE_LANDMARKS` parses only type-1, 2D feature points in the Landmark Point
Block of an ISO/IEC 19794-5:2005 record already stored as `FACE`. It recognizes
the documented bilateral eye-corner, eye-centre, nose, and nostril codes plus
the supported nose midline code. Unknown or duplicate codes, incomplete
bilateral pairs, multiple face blocks, or fewer than two known pairs are
rejected rather than guessed. The returned coordinate-space identifier is the
stable UUID `19794520-0500-5000-8000-000000000002`. ISO/IEC 39794-5 remains
structurally validated ASN.1 and is not decoded into landmarks in this release.

## Indexed search

```sql
CREATE INDEX identity_face_idx
  ON identities (face_features) USING HNSW;
CREATE INDEX identity_iris_idx
  ON identities (iris_code) USING HAMMING;
CREATE INDEX identity_print_idx
  ON identities (fingerprint) USING MINUTIAE;

SEARCH FEATURE $1 IN identities COLUMN face_features LIMIT 10 MIN_SCORE 0.80;
SEARCH BITS $1 IN identities COLUMN iris_code LIMIT 10 MIN_SCORE 0.80;
SEARCH FINGERPRINT $1 IN identities COLUMN fingerprint LIMIT 10 MIN_SCORE 0.80;
```

Add `EXACT` to force a full scan. Otherwise, the index supplies candidates and
the exact comparator reranks them. Ties are ordered by RedDB entity ID.

The first indexed feature value fixes the index contract: `space_id` plus
dimension for HNSW, `space_id` plus byte length for Hamming, or matcher version
for minutiae. Incompatible writes fail before row publication. A persisted
index whose matcher version is old is ignored by query planning; exact search
remains available until the index is rebuilt.

## Typed HTTP parameters

`POST /query` accepts these JSON envelopes in the `params` array:

```json
{
  "$biometric": {
    "modality": "face",
    "format": "ISO/IEC 39794-5",
    "data": "3003020101"
  }
}
```

```json
{
  "$feature_vector": {
    "space_id": "00112233445566778899aabbccddeeff",
    "values": [0.25, 0.75]
  }
}
```

```json
{
  "$bit_signature": {
    "space_id": "00112233445566778899aabbccddeeff",
    "bits": "a55a",
    "mask": "fff0"
  }
}
```

```json
{
  "$landmark_set": {
    "space_id": "00112233445566778899aabbccddeeff",
    "dimensions": 2,
    "pairs": [
      { "left": [10.0, 20.0], "right": [30.0, 20.0] },
      { "left": [14.0, 30.0], "right": [26.0, 30.0] }
    ],
    "midline": [[20.0, 36.0]]
  }
}
```

Binary fields use lowercase or uppercase hexadecimal on input. The mask is
optional in SQL construction and typed HTTP/gRPC input, and defaults to all
valid bits; typed transport output always includes the effective mask.

The persisted and wire identifiers are compatibility contracts:
`LANDMARK_SET` uses portable type tag `62`, native value tag `59`, file
capability bit `1 << 8`, RedWire value tag `0x0f`, RedWire capability
`0x00000010`, and gRPC `QueryValue` field `16`. PostgreSQL wire returns its
typed JSON envelope as JSONB. Older file readers fail closed when the file
requires the landmark capability, and RedWire clients negotiate the capability
before emitting tag `0x0f`.

## Privacy and security boundary

Biometric data is sensitive personal data. Use transport encryption,
encryption at rest, narrow column privileges, RLS, retention policies, and
application-level consent/audit controls appropriate to the deployment.
RedDB validates and stores records, but it does not decide whether collection
or matching is legally permitted.

Diagnostics report only bounded metadata and validation categories. They do
not include coordinates, bilateral pairs, embeddings, bit signatures, or raw
biometric payloads.

<!-- contract-matrix:begin -->
## Public-surface support

> Generated from [`docs/conformance/public-surface-contract-matrix.json`](/docs/conformance/public-surface-contract-matrix.json) by `scripts/gen-docs-from-matrix.mjs`. Do not edit between the markers by hand — run `node scripts/gen-docs-from-matrix.mjs --write`. The matrix is the source of truth; this block can never claim more than it, and CI (`docs-matrix`) fails on drift.
>
> The public promises this document makes, and the status of each surface.

| Promise | sql | http | redwire | grpc | driver_helpers |
| --- | --- | --- | --- | --- | --- |
| **PSC-015** — Standardized fingerprint, face, and iris records plus caller-supplied feature vectors, bit signatures, and landmark sets are native typed values with deterministic comparison, symmetry scoring, and indexed search where applicable. | ✅ supported | ⚠️ partial | ⚠️ partial | ⚠️ partial | ⚠️ partial |

_Status legend: ✅ supported · ⚠️ partial (known gaps) · ❌ unsupported._
<!-- contract-matrix:end -->
