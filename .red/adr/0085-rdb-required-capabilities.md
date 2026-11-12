# ADR 0085 — Required capabilities in paged `.rdb` superblocks

Status: accepted
Date: 2026-11-10

## Context

The paged format historically used one monotonically increasing version for
both structural changes and reader requirements. Version 1.0.4 also implicitly
meant that a disk-resident writer was required. That coupling prevents an
independent codec such as native `TEMPERATURE` from evolving the file without
being mistaken for a disk-residency change. It also makes an unsupported value
fail only after a reader reaches the record that contains its tag.

MCU storage adapters add another constraint: flash geometry, erase strategy,
and zero-block compression are properties of the physical device, but the
logical `.rdb` must remain readable by the canonical engine.

## Decision

- Paged format 1.0.9 adds a `required_capabilities` bitmap to the existing
  database header in each superblock copy. The page size and format version
  remain explicit header fields. Capability bits are monotonic for a file.
- Bit 0 is the native temperature codec. Bit 1 is bounded disk residency.
  Readers reject unknown required bits while decoding the selected superblock,
  before scanning data pages.
- Bits 2 and 4 bind copy-on-write catalogs and tree-image proofs respectively.
  Bit 3 keeps recovery output with unknown identity floors read-only. Bit 5
  requires independent DWB frame publication ([ADR 0084](0084-independent-dwb-frame-publication.md)).
- A writer durably publishes a new required capability to both recoverable
  superblock copies before it publishes a schema or value that depends on the
  capability. If either fence write fails, further writes remain fenced until
  reopen and recovery.
- Versions before 1.0.9 retain their original interpretation. Versions 1.0.4
  through 1.0.6 and 1.0.8 require disk residency; 1.0.7 is the resident
  result-cache format. When such a file crosses the 1.0.9 boundary, the writer
  translates that requirement into bit 1.
- `TEMPERATURE` is canonical RedDB code, not an MCU-only codec. Its portable
  datatype tag is 56 and native-store value tag is 53. Exact comparison and
  hashing use rational Kelvin while storage preserves the input unit and scale.
- MCU VFS strategies may change physical I/O, erase amplification, and sparse
  block representation. They must not change the logical page size, tags,
  superblocks, or decoded `.rdb` byte stream.

## Consequences

- Adding a codec no longer opts a database into unrelated storage strategies.
- An older reader either understands every required bit or rejects the file at
  open; it does not discover incompatibility halfway through a query.
- A conservatively declared but unused capability can survive a crash. The
  inverse state — dependent bytes without the capability — is forbidden.
- The capability fence costs two small synchronous superblock writes only when a
  file first enables a capability, not per row.
- Cross-platform compatibility is tested at the logical `.rdb` boundary. MCU
  flash containers and host filesystems may differ below that boundary.
