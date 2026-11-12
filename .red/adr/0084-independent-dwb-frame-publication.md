# ADR 0084 — Independent publication authority for DWB frames

Status: proposed
Date: 2026-11-12

## Context

An old complete double-write frame can pass its own CRC. Replaying it before
validating native tree proofs overwrites current homes and consumes the frame;
rejecting the resulting read afterward does not preserve recovery evidence.
The native SQL regression reproduces this using a reused MVCC page address.

A frame digest alone cannot distinguish interrupted staging from a damaged
frame needed to repair interrupted home writes, or from interrupted clearing.
Recovery itself must establish the same phase fences before touching homes.

## Decision

- Required-capability bit 5 declares independently authorized DWB publication.
  Both superblock copies cross the capability fence before guarded writes.
  Native proof preparation and offline proof reconstruction enable this bit.
- Bytes 296..336 of each superblock contain a phase (`u32`), encoded frame
  length (`u32`) and BLAKE3 digest (32 bytes). Zero is an unused publication.
  The digest covers entry addresses and complete raw page bytes, including
  encrypted images. A frame's own CRC is not freshness authority.
- Before staging, publish `Preparing` in both slots. Stage and sync the full
  bounded frame, then publish `Ready` in both slots before writing any homes.
  Sync homes, publish `Consumed` in both slots, then clear and sync the zone.
  Complete this sequence for each frame before preparing another frame.
- A complete frame with a different digest is rejected before changing the
  source. `Ready` also rejects missing or incomplete evidence. `Preparing`
  can discard interrupted staging because home writes were not eligible;
  `Consumed` can finish interrupted clearing after the home barrier or the
  abandonment of an untouched stage. A consumed frame is never replayed.
- Recovery validates the independent digest and addresses before any write.
  It repeats both `Ready` fences before replay, including when the selected
  fallback is `Preparing`, and both `Consumed` fences before clearing. Failed
  recovery may be restarted after another crash without replacing its frame.
- Read-only recovery applies authorized images only to its private view.
  Offline recovery uses the same validation, copies the source first and
  completes mutation on that copy. Neither path authorizes a frame by its
  own checksum or by the caller's willingness to repair the database.
- The native pager serializes phase publication with ordinary superblock
  publication, including eviction writes. Ordinary header encoding preserves
  the extension from the newest slot instead of a cached publication value.
  Guarded files reject mutable handles configured without DWB protection.
- Legacy files retain their previous DWB interpretation until the capability
  is fenced. Unknown mandatory bits make older readers reject the file.
  Tree proofs remain responsible for detecting wrong home images; a consumed
  phase is not a substitute for verifying authoritative data on read.

## Consequences and verification

Each bounded frame adds six small sealed-slot writes and their fsync barriers.
It also clears between frames rather than only after the final legacy frame.
The protocol uses the existing DWB zone and superblock slots, adds no sidecar
and requires no whole-database scan at startup. This favors explicit durability
ordering; batching remains the way to amortize the additional barriers.

Portable integration tests interrupt every write/sync, retain or discard
unflushed bytes, destroy either superblock copy, and require exact post-Ready
images and a subsequent durable write. Additional journeys interrupt recovery
again and recover a final frame without undoing earlier frames. Native tests
require stale-frame rejection with unchanged source/WAL, authorized current
repair, HASH/BTREE parity, UNIQUE enforcement and bounded forward progress.
Offline tests cover interrupted staging and clearing on a preserved copy.

The legacy migration/retirement journey now verifies the first protected
publication, either-superblock fallback, exact cold reads and subsequent
update/vacuum/overflow recreation cycles with actual address reuse. Retained
legacy trees receive independent proofs without moving their data; existing
reader pins continue to prevent premature reclamation. Native and offline
readers reject missing retired descriptors in a proof-bearing catalog.
