# SIMD Optimization

RedDB uses SIMD (Single Instruction, Multiple Data) instructions to accelerate vector distance computations.

## Supported Operations

| Operation | SIMD Acceleration |
|:----------|:-----------------|
| Cosine similarity | Yes |
| Euclidean distance (L2) | Yes |
| Dot product | Yes |
| Vector normalization | Yes |

## How It Works

SIMD processes multiple float values in a single CPU instruction:

```
Scalar: a[0]*b[0], a[1]*b[1], a[2]*b[2], a[3]*b[3]  (4 instructions)
SIMD:   a[0:3] * b[0:3]                                (1 instruction)
```

For a 768-dimensional vector, SIMD provides approximately 4-8x speedup on distance calculations.

## Architecture Support

RedDB's SIMD implementation adapts to the available CPU features:

| Architecture | SIMD Width | Speedup |
|:------------|:-----------|:--------|
| x86_64 (SSE4.1) | 128-bit (4 floats) | ~4x |
| x86_64 (AVX2) | 256-bit (8 floats) | ~8x |
| aarch64 (NEON) | 128-bit (4 floats) | ~4x |
| Fallback | Scalar | 1x |

## Automatic Detection

The engine detects available SIMD features at runtime and selects the fastest code path. No configuration is needed.

## Impact on Search Performance

For a collection of 100K vectors (768-dim):

| Method | Time per Query |
|:-------|:--------------|
| Scalar | ~50ms |
| SSE4.1 | ~12ms |
| AVX2 | ~6ms |

> [!TIP]
> SIMD acceleration is most impactful for flat (brute-force) searches and the re-ranking phase of IVF and HNSW searches.
