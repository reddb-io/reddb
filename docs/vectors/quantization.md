# Quantization (PQ, Binary, Int8)

Quantization reduces vector memory usage while maintaining search quality. RedDB supports product quantization, binary quantization, and int8 quantization.

## Product Quantization (PQ)

PQ divides vectors into sub-vectors and quantizes each independently:

1. Split each vector into `m` sub-vectors
2. Cluster each sub-vector space into 256 centroids
3. Replace each sub-vector with its centroid index (1 byte)

**Result**: A 768-dim float32 vector (3072 bytes) becomes 96 bytes (m=96) -- a 32x compression.

### Trade-offs

| Metric | Before PQ | After PQ (m=96) |
|:-------|:----------|:-----------------|
| Memory per vector | 3072 bytes | 96 bytes |
| Search speed | Baseline | 5-10x faster |
| Recall@10 | 100% | 90-95% |

## Binary Quantization

Reduces each float dimension to a single bit:

- Positive values become `1`
- Negative/zero values become `0`

**Result**: A 768-dim vector becomes 96 bytes (768 bits).

Binary quantization is fastest for initial candidate filtering, followed by re-ranking with full vectors.

## Int8 Quantization

Maps float32 values to int8 (-128 to 127):

1. Compute per-dimension min/max
2. Linearly map to int8 range

**Result**: A 768-dim vector goes from 3072 bytes to 768 bytes -- a 4x compression with minimal quality loss.

## Comparison

| Method | Compression | Quality | Speed |
|:-------|:-----------|:--------|:------|
| None (float32) | 1x | Perfect | Baseline |
| Int8 | 4x | Very high (99%+) | 2-3x |
| PQ | 32x | Good (90-95%) | 5-10x |
| Binary | 32x | Moderate (80-90%) | 10-20x |

## Usage

Quantization is applied transparently by the vector engine based on collection size and available memory. The tiered search system selects the best strategy automatically.

See [Tiered Search](/vectors/tiered-search.md) for how quantization integrates with the search pipeline.
