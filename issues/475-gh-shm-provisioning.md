---
status: open
tag: AFK
gh: 475
---

# [AFK] gh-475: Provision -shm shared memory file for standard tier (SQLite WAL mode parity)

GitHub: reddb-io/reddb#475

## What to build

When tier=standard: create + mmap an `-shm` file at open. Multiple embedded readers must coexist. Crash detection on next open. Document lock protocol + binary layout in ADR.

## Acceptance criteria

- [ ] ADR child or ADR-0018 section documents lock protocol + binary `-shm` layout
- [ ] `-shm` created + mmap-ed when tier=standard
- [ ] Multiple readers in embedded operate simultaneously
- [ ] Crash of one process detected and state cleaned on next open
- [ ] Concurrency tests cover happy path + crash recovery

## Notes
- `CARGO_TARGET_DIR=.target-gh475`
- Commit `Closes #475` or `Refs` if partial
- Land the file/layout half + tests; runtime auto-enable blocked on tier wiring.

## Progress (2026-05-16)

Landed (file/layout half):
- `physical/shm.rs`: 64-byte binary header (magic `RDBSHM01`, version,
  owner_pid, generation, reader_count, last_heartbeat_ms, FNV-1a
  checksum). One-page (4 KiB) file size. `provision_shm()` covers all
  four outcome states: `Created`, `AttachedToLiveOwner`,
  `RecoveredFromCrash`, `HealedCorruptHeader`. Unix liveness probe via
  `kill(pid, 0)` with EPERM→alive fallback.
- Process-global opt-in toggle (`set_shm_provisioning_enabled`) +
  `REDDB_SHM_PROVISION` env escape hatch, mirroring gh-472 / gh-473.
- ADR-0018: appended "`<data>.shm` shared-memory substrate" section
  with binary layout table + lock protocol + non-goals.
- `tests/e2e_shm_provisioning.rs`: 6 cases — default-off, create,
  same-pid reattach (no generation bump), multi-reader attach/detach,
  crash recovery (forged dead pid past `pid_max`), corrupt header heal.

Blocked on the same tier wiring as gh-471 / gh-472 / gh-473:
- mmap step (no `memmap2` dep in tree); deferred per ADR non-goals.
- Tier-driven auto-enable at `RedDBRuntime::open` for tier ≥ Standard.

Cargo build/test could not be run inside the sandbox (`cargo check`
requires user approval in this environment). Next iteration should run
`CARGO_TARGET_DIR=.target-gh475 cargo test --test e2e_shm_provisioning`.
