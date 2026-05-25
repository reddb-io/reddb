# ADR 0024: TurboQuant encoded-codes storage layout — per-vector vs blocked-by-32

Status: Accepted (2026-05-25)

Related: PRD #668 (vector.turbo TurboQuant index), issues #670, #671, #672, #673, #685.

## Context

PRD #668 splits TurboQuant scoring into four implementation slices — NEON (#670), AVX2 (#671), AVX-512BW (#672), and crash-safety / background rebuild (#673). Slices B/C/D were attempted in parallel by independent AFK workers, and each invented its own dispatcher API and, critically, its own assumption about how encoded 4-bit codes are laid out in memory.

Slice B (NEON, merged on `main` as `1addd82d`) chose a **per-vector** layout: each `EncodedVector` owns its own `packed: Vec<u8>`. The NEON kernel gathers from N separate pointers per group into a 32-lane temp buffer before applying SIMD intrinsics. The insert path is simple — write each vector's bytes into its own owned buffer — but every SIMD query pays a per-group gather over N pointers.

Slices C (AVX2) and D (AVX-512BW), preserved on `afk/wZ8F0/671-...` (`c3c615d2`) and `afk/wZ8F0/672-...` (`8ea1277f`), chose a **blocked-by-32** layout faithful to the upstream turbovec design (RyanCodrai/turbovec @ `4a4f2cd2`): codes of 32 consecutive vectors are pre-interleaved in one buffer using a PERM0 permutation. The AVX2 / AVX-512BW kernels read aligned 256-bit / 512-bit registers directly, with no per-query repack. Insert is more complex — vectors must be assigned to 32-vector blocks and interleaved at write time, with the trailing partial block padded — but the hot path is gather-free.

The two layouts are not a code-style difference. They change what the storage subsystem persists for encoded codes, what `EncodedVector` owns, and what the SIMD kernels can assume about alignment and stride.

## Decision

**Adopt blocked-by-32 as the canonical encoded-codes layout.** Codes for 32 consecutive vectors are pre-interleaved with the PERM0 permutation into one packed buffer owned by collection storage, with a trailing partial block at the end. `EncodedVector` becomes an index into that buffer, not a per-vector `Vec<u8>` owner. All SIMD kernels (NEON, AVX2, AVX-512BW) consume aligned register-width slices directly with no per-query repack.

Rejected: per-vector layout with repack-on-the-fly inside SIMD kernels. Functional, but every SIMD query pays a 32-lane gather/repack and the design fails the perf gate the PRD attaches to slices C and D. Landing it would force a redesign later under more pressure, not less.

Slice B (#670) was merged on `main` as `1addd82d` under the rejected per-vector layout and must be reverted before slices C and D can land on the canonical layout. Slice E (#673, crash-safety + background rebuild) is independent of this decision and can proceed in parallel; its crash-recovery contract must, however, cover the partial-block tail introduced by this ADR.

## Consequences

- `main` commit `1addd82d` (slice B merge) is reverted; the NEON code in `crates/reddb-server/src/storage/engine/turboquant/scoring.rs` is rewritten against the blocked layout before re-merging.
- `EncodedVector` no longer owns a per-vector `packed: Vec<u8>`. Collection storage owns one packed buffer per 32-vector block plus a tail of trailing codes; `EncodedVector` becomes `(block_idx, lane)`.
- The insert path must place new vectors into existing partial blocks or open a new one, and must guarantee that writes within a block are visible to SIMD reads as a unit. Crash-safety semantics (slice E, #673) cover the partial-block tail explicitly.
- The preserved branches `afk/wZ8F0/671-...` (`c3c615d2`) and `afk/wZ8F0/672-...` (`8ea1277f`) are written against this layout already and can be cherry-picked onto the redesign branch without their `score.rs` / `scoring.rs` divergence carrying over — the redesign consolidates on `scoring.rs`.
- NEON paired-block parity tests cannot run on x86 CI hosts; they remain `#[cfg(target_arch = "aarch64")]`-gated and must be exercised on an ARM runner before PRD #668 closes.

## References

- Issue #685 — decision tracking and discussion thread for this ADR.
- PRD #668 — vector.turbo TurboQuant index, slice plan.
- `afk/wZ8F0/670-...` `6672436a` — slice B NEON, per-vector layout, merged on `main` as `1addd82d` (to be reverted).
- `afk/w4OWQ/670-...` `29dc3a13` — alternative slice B NEON attempt (`neon.rs`), not merged, kept for reference.
- `afk/wZ8F0/671-...` `c3c615d2` — slice C AVX2, blocked layout, preserved.
- `afk/wZ8F0/672-...` `8ea1277f` — slice D AVX-512BW, blocked layout, preserved.
- Upstream turbovec MIT reference: RyanCodrai/turbovec @ `4a4f2cd2db233f24405911b1ceaf1823fa23b4ac`.
