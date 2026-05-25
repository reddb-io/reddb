# ADR 0024: TurboQuant encoded-codes storage layout — per-vector vs blocked-by-32

Status: Proposed (2026-05-25)

Related: PRD #668 (vector.turbo TurboQuant index), issues #670, #671, #672, #673, #685.

## Context

PRD #668 splits TurboQuant scoring into four implementation slices — NEON (#670), AVX2 (#671), AVX-512BW (#672), and crash-safety / background rebuild (#673). Slices B/C/D were attempted in parallel by independent AFK workers, and each invented its own dispatcher API and, critically, its own assumption about how encoded 4-bit codes are laid out in memory.

Slice B (NEON, merged on `main` as `1addd82d`) chose a **per-vector** layout: each `EncodedVector` owns its own `packed: Vec<u8>`. The NEON kernel gathers from N separate pointers per group into a 32-lane temp buffer before applying SIMD intrinsics. The insert path is simple — write each vector's bytes into its own owned buffer — but every SIMD query pays a per-group gather over N pointers.

Slices C (AVX2) and D (AVX-512BW), preserved on `afk/wZ8F0/671-...` (`c3c615d2`) and `afk/wZ8F0/672-...` (`8ea1277f`), chose a **blocked-by-32** layout faithful to the upstream turbovec design (RyanCodrai/turbovec @ `4a4f2cd2`): codes of 32 consecutive vectors are pre-interleaved in one buffer using a PERM0 permutation. The AVX2 / AVX-512BW kernels read aligned 256-bit / 512-bit registers directly, with no per-query repack. Insert is more complex — vectors must be assigned to 32-vector blocks and interleaved at write time, with the trailing partial block padded — but the hot path is gather-free.

The two layouts are not a code-style difference. They change what the storage subsystem persists for encoded codes, what `EncodedVector` owns, and what the SIMD kernels can assume about alignment and stride.

## Decision

**Open.** This ADR records the trade-off and the constraints; the decision is recorded in issue #685.

The two viable choices:

1. **Adopt blocked-by-32 as the canonical layout.** Revert slice B's per-vector merge, rework the NEON kernel against the blocked layout, and land C and D on that shared foundation. One layout for all kernels, perf consistent with the PRD #668 SIMD targets, redo cost on the slice-B work already merged.
2. **Keep per-vector and port C/D with repack-on-the-fly.** Smallest delta to land AVX2/AVX-512BW immediately, but every SIMD query pays a 32-lane repack inside the kernel; perf gates are expected to reject this and force a redesign later.

Option 2 is not durable. Option 1 is the technically correct path.

Slice E (#673, crash-safety + background rebuild) is independent of this decision and can proceed regardless.

## Consequences

If option 1 is chosen:
- `main` commit `1addd82d` (slice B merge) is reverted; the NEON code in `crates/reddb-server/src/storage/engine/turboquant/scoring.rs` is rewritten against the blocked layout before re-merging.
- `EncodedVector` no longer owns a per-vector `packed: Vec<u8>`. The collection-level storage owns one packed buffer per 32-vector block plus a tail of trailing codes; `EncodedVector` becomes an index into that buffer.
- The insert path must place new vectors into existing partial blocks or open a new block, and writes must be flushed in 32-vector batches for the SIMD reads to see consistent state. Crash-safety semantics (slice E) must cover the partial-block tail.
- The preserved branches `afk/wZ8F0/671-...` and `afk/wZ8F0/672-...` can be cherry-picked into the new layout; their kernels are already written against it.

If option 2 is chosen:
- Slices C and D land soon, but the perf benchmarks attached to PRD #668 are expected to fail the SIMD speedup gate. A follow-up redesign issue must be filed at merge time, not after the gate is missed.

## References

- Issue #685 — decision tracking and discussion thread for this ADR.
- PRD #668 — vector.turbo TurboQuant index, slice plan.
- `afk/wZ8F0/670-...` `6672436a` — slice B NEON, per-vector layout, merged.
- `afk/wZ8F0/671-...` `c3c615d2` — slice C AVX2, blocked layout, preserved.
- `afk/wZ8F0/672-...` `8ea1277f` — slice D AVX-512BW, blocked layout, preserved.
- Upstream turbovec MIT reference: RyanCodrai/turbovec @ `4a4f2cd2db233f24405911b1ceaf1823fa23b4ac`.
